//! Against a real server — only when asked:
//!
//! ```text
//! SMB_LIVE=127.0.0.1:445 [SMB_LIVE_NAME=host] [SMB_LIVE_USER=u SMB_LIVE_PASS=p]
//! [SMB_LIVE_SHARE=share] cargo test -p smb --test live -- --nocapture
//! ```
//!
//! Without `SMB_LIVE` the test does nothing. With it and no user, it proves
//! negotiate and the NTLM exchange up to the server's verdict (anonymous
//! share listing, or LOGON_FAILURE for a made-up account).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use smb::client::{Auth, Client, Transport};
use smb::{Error, ntlm, smb2};

struct Tcp {
    stream: TcpStream,
    seed: u64,
}

impl Transport for Tcp {
    fn send(&mut self, data: &[u8]) -> Result<(), Error> {
        self.stream.write_all(data).map_err(|_| Error::Io)
    }

    fn recv_exact(&mut self, buf: &mut [u8]) -> Result<(), Error> {
        self.stream.read_exact(buf).map_err(|_| Error::Closed)
    }

    fn random(&mut self, out: &mut [u8]) {
        for byte in out {
            self.seed ^= self.seed << 13;
            self.seed ^= self.seed >> 7;
            self.seed ^= self.seed << 17;
            *byte = self.seed as u8;
        }
    }

    fn filetime(&self) -> u64 {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        smb::wire::unix_to_filetime(now)
    }
}

fn connect(address: &str, name: &str) -> Client<Tcp> {
    let stream = TcpStream::connect(address).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64 | 1;
    let client = Client::connect(Tcp { stream, seed }, name).expect("negotiate");
    println!(
        "dialect {} signing required {} max read {}",
        smb2::dialect_name(client.dialect),
        client.server_requires_signing(),
        client.max_read
    );
    client
}

#[test]
fn live_server() {
    let Ok(address) = std::env::var("SMB_LIVE") else {
        return;
    };
    let name = std::env::var("SMB_LIVE_NAME").unwrap_or_else(|_| address.split(':').next().unwrap().to_string());

    // Anonymous: share listing, or the server's refusal.
    let mut client = connect(&address, &name);
    match client.login(&Auth::Anonymous, "FOSSAI") {
        Ok(()) => {
            println!("anonymous session, server {:?}", client.server_name);
            match client.shares() {
                Ok(shares) => println!("anonymous shares: {shares:?}"),
                Err(err) => println!("anonymous share list refused: {err}"),
            }
        }
        Err(err) => println!("anonymous refused: {err}"),
    }

    // A made-up account must be refused with an authentication status — the
    // NTLMv2 exchange reached the server's verdict.
    let mut client = connect(&address, &name);
    let bogus = ntlm::Identity { user: "fossai-no-such-user".into(), domain: String::new(), nt_hash: ntlm::nt_hash("wrong") };
    let verdict = client.login(&Auth::User(bogus), "FOSSAI");
    println!("bogus account: {verdict:?}");
    if let Err(err) = verdict {
        assert!(err.is_auth() || matches!(err, Error::Status(_)), "unexpected: {err}");
    }

    let (Ok(user), Ok(pass)) = (std::env::var("SMB_LIVE_USER"), std::env::var("SMB_LIVE_PASS")) else {
        return;
    };
    let (user, domain) = ntlm::split_user(&user);
    let mut client = connect(&address, &name);
    client.login(&Auth::User(ntlm::Identity { user, domain, nt_hash: ntlm::nt_hash(&pass) }), "FOSSAI").expect("login");
    println!("logged on, guest/null: {}", client.is_guest_or_null());
    let shares = client.shares().expect("share list");
    println!("shares: {shares:?}");
    let share = std::env::var("SMB_LIVE_SHARE").unwrap_or_else(|_| shares.iter().find(|s| s.is_visible_disk()).expect("a disk share").name.clone());
    let tree = client.tree_connect(&share).expect("tree connect");
    let root = client.create(tree, "", smb2::ACCESS_LIST, smb2::FILE_OPEN, smb2::FILE_DIRECTORY_FILE, 0).expect("open root");
    let entries = client.list(tree, root.id).expect("list");
    client.close(tree, root.id).unwrap();
    println!("{} entries: {:?}", entries.len(), entries.iter().take(10).map(|e| &e.name).collect::<Vec<_>>());
    if let Ok(space) = client.space(tree) {
        println!("space {space:?}");
    }

    // Write, read back, rename, delete — on a scratch name.
    let file = client
        .create(tree, "fossai-smb-test.txt", smb2::ACCESS_WRITE | smb2::DELETE, smb2::FILE_OVERWRITE_IF, smb2::FILE_NON_DIRECTORY_FILE, smb2::ATTR_NORMAL)
        .expect("create");
    let text = b"FossAi SMB test\n".repeat(5000);
    let mut at = 0usize;
    while at < text.len() {
        at += client.write(tree, file.id, at as u64, &text[at..]).expect("write");
    }
    let mut back = Vec::new();
    loop {
        let (chunk, _) = client.read(tree, file.id, back.len() as u64, 65536).expect("read");
        if chunk.is_empty() {
            break;
        }
        back.extend_from_slice(&chunk);
    }
    assert_eq!(back, text);
    client.close(tree, file.id).unwrap();
    client.rename(tree, "fossai-smb-test.txt", "fossai-smb-test2.txt").expect("rename");
    let info = client.stat(tree, "fossai-smb-test2.txt").expect("stat");
    assert_eq!(info.size, text.len() as u64);
    client.delete(tree, "fossai-smb-test2.txt", false).expect("delete");
    assert!(client.stat(tree, "fossai-smb-test2.txt").is_err());
    client.tree_disconnect(tree).unwrap();
    client.logoff();
    println!("live test passed");
}
