//! «Сеть» in «Файлах» (24.11): shared folders of other computers over SMB.
//!
//! The kernel shows them as files (`fs::netfs`): `/net` — the computers found
//! in the network, `/net/<host>` — its shared folders, below — their files.
//! What is left to the file manager:
//!
//! * **listings in the background.** A folder under `/net` is read by a child
//!   `files --netlist OUT PATH` (the heap of the program is single-threaded,
//!   so not a thread): connecting to a computer that does not answer takes
//!   seconds, and the window stays alive, showing «Загрузка…»;
//! * **the password.** A refusal `ERR_PERMISSION` means «the server wants an
//!   account»: `/bin/netlogon PATH` asks for it (the window «Введите сетевые
//!   учётные данные»), and with code 0 the folder is read again;
//! * **names as in Windows**: `\\host\share` in the address bar, «Сеть», a
//!   computer and a shared folder with their own pictures.

use super::*;

/// A listing of a network folder in a child process.
pub(super) struct NetLoad {
    task: i64,
    path: String,
    out: String,
    started: u64,
}

/// `/bin/netlogon` asking for the account of a network folder.
pub(super) struct NetLogin {
    task: i64,
    path: String,
    /// `netlogon` has closed. A refusal of the same folder after that is
    /// said, not asked again: `netlogon` answers 0 also on its cancel, and
    /// asking anew would bring the window back for ever.
    closed: bool,
}

/// Biggest answer file of the child (1024 rows of a long name).
const NETLIST_LIMIT: usize = 1 << 20;

/// Numbering of the answer files of one window.
static NETLIST_SEQ: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// `/net` or under it.
pub(super) fn is_net(path: &str) -> bool {
    path == sysconf::winpath::NET || path.starts_with("/net/")
}

/// Depth under `/net`: 0 — the computers, 1 — shared folders of a computer,
/// 2 — inside a shared folder. `None` — not a network path.
pub(super) fn net_level(path: &str) -> Option<usize> {
    if !is_net(path) {
        return None;
    }
    Some(path.trim_end_matches('/').split('/').filter(|part| !part.is_empty()).count().saturating_sub(1).min(2))
}

/// How a network path is shown: «Сеть» or `\\host\share\dir`.
pub(super) fn net_title(path: &str) -> String {
    sysconf::winpath::net_to_unc(path).unwrap_or_else(|| String::from("Сеть"))
}

/// `files --netlist OUT PATH…`: list PATH into OUT and exit. First line
/// `ok\t<dropped>` or `err\t<code>`; then `<d|f>\t<mode>\t<size>\t<mtime>\t<name>`.
pub(super) fn netlist_main(args: &Args) -> ! {
    let Some(out) = args.get(2) else {
        user_progs::exit(2);
    };
    let path = (3..args.len()).filter_map(|index| args.get(index)).collect::<Vec<_>>().join(" ");
    let mut text = String::new();
    match list_dir_code(&path) {
        Ok((rows, dropped)) => {
            text.push_str(&format!("ok\t{dropped}\n"));
            for row in rows {
                let kind = if row.directory { 'd' } else { 'f' };
                text.push_str(&format!("{kind}\t{}\t{}\t{}\t{}\n", row.mode, row.size, row.mtime, row.name));
            }
        }
        Err((code, _)) => text.push_str(&format!("err\t{code}\n")),
    }
    println(&format!("files: netlist {path}: {}", text.lines().next().unwrap_or("")));
    let code = if write_file(out, text.as_bytes()).is_ok() { 0 } else { 1 };
    user_progs::exit(code)
}

/// The answer file of the child, read in 64 KiB pieces (one read of the
/// whole [`NETLIST_LIMIT`] is refused by the kernel and read as nothing).
fn read_answer(path: &str) -> String {
    let fd = open(path);
    if fd < 0 {
        return String::new();
    }
    let mut bytes = Vec::new();
    let mut chunk = alloc::vec![0u8; 64 * 1024];
    while bytes.len() < NETLIST_LIMIT {
        let got = read(fd, &mut chunk);
        if got <= 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..got as usize]);
    }
    close(fd);
    String::from_utf8(bytes).unwrap_or_default()
}

/// Rows of a `--netlist` answer: `Ok((rows, dropped))` or the refusal.
fn parse_netlist(text: &str, path: &str) -> Result<(Vec<Row>, usize), i64> {
    let mut lines = text.lines();
    let head = lines.next().unwrap_or("err\t-8");
    let (status, value) = head.split_once('\t').unwrap_or((head, ""));
    if status != "ok" {
        return Err(value.parse().unwrap_or(user_abi::ERR_IO));
    }
    let dropped = value.parse().unwrap_or(0);
    let program = path.ends_with("/bin");
    let mut rows = Vec::new();
    for line in lines {
        let mut fields = line.splitn(5, '\t');
        let (Some(kind), Some(mode), Some(size), Some(mtime), Some(name)) = (fields.next(), fields.next(), fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let mut entry = Dirent::default();
        entry.mode = mode.parse().unwrap_or(0);
        entry.size = size.parse().unwrap_or(0);
        entry.mtime = mtime.parse().unwrap_or(0);
        rows.push(Row::entry(name, kind == "d", program, &entry));
    }
    Ok((rows, dropped))
}

impl Files {
    /// Read the open network folder in the background (see the module).
    pub(super) fn start_net_load(&mut self) {
        if self.net_load.as_ref().is_some_and(|load| load.path == self.path) {
            return;
        }
        self.net_load = None;
        // The account asked for another folder no longer matters.
        if self.net_login.as_ref().is_some_and(|login| login.closed && login.path != self.path) {
            self.net_login = None;
        }
        let seq = NETLIST_SEQ.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let out = format!("/tmp/.files-net-{}-{seq}", user_progs::pid());
        let task = spawn(&format!("/bin/files --netlist {out} {}", self.path));
        if task < 0 {
            // No child — read here (the window waits, as before 24.11).
            println(&format!("files: netlist child failed ({task}); reading {} in place", self.path));
            match list_dir_code(&self.path) {
                Ok((rows, dropped)) => self.adopt_rows(rows, dropped),
                Err((code, _)) => self.net_refused(code),
            }
            return;
        }
        println(&format!("files: loading {} in the background (task {task})", self.path));
        self.net_load = Some(NetLoad { task, path: self.path.clone(), out, started: monotonic_ms() });
    }

    /// Each turn of the loop: the child finished listing, `netlogon` closed.
    pub(super) fn net_step(&mut self) -> bool {
        let mut changed = false;
        if let Some(load) = self.net_load.as_ref() {
            let code = user_progs::wait_now(load.task);
            if code != user_abi::ERR_AGAIN {
                let load = self.net_load.take().expect("checked above");
                let text = read_answer(&load.out);
                if text.is_empty() {
                    println(&format!("files: netlist answer {} missing (child exit {code})", load.out));
                }
                let _ = remove(&load.out);
                let took = monotonic_ms().saturating_sub(load.started);
                if load.path == self.path {
                    match parse_netlist(&text, &load.path) {
                        Ok((rows, dropped)) => {
                            self.adopt_rows(rows, dropped);
                            println(&format!("files: network folder {} read in {took} ms, {} item(s)", load.path, self.rows.len()));
                        }
                        Err(code) => {
                            println(&format!("files: network folder {} refused: {code} ({took} ms)", load.path));
                            self.net_refused(code);
                        }
                    }
                }
                changed = true;
            }
        }
        let finished = match self.net_login.as_mut().filter(|login| !login.closed) {
            Some(login) => {
                let code = user_progs::wait_now(login.task);
                (code != user_abi::ERR_AGAIN).then(|| {
                    login.closed = true;
                    (login.path.clone(), code)
                })
            }
            None => None,
        };
        if let Some((path, code)) = finished {
            println(&format!("files: netlogon for {path} closed with {code}"));
            if path == self.path {
                if code == 0 {
                    self.reload();
                } else {
                    self.error = Some(format!("Нет доступа к {}: вход отменён", net_title(&path)));
                }
            }
            changed = true;
        }
        changed
    }

    /// A network folder refused: a password is asked once (`netlogon`),
    /// anything else is said in words.
    fn net_refused(&mut self, code: i64) {
        if code == user_abi::ERR_PERMISSION {
            if self.net_login.as_ref().is_some_and(|login| login.path == self.path) {
                // Asked already and still refused: cancelled (or closed).
                println(&format!("files: {} still refused after netlogon", self.path));
                self.error = Some(format!("Нет доступа к {}: вход отменён", net_title(&self.path)));
                return;
            }
            let task = spawn(&format!("/bin/netlogon {}", self.path));
            if task >= 0 {
                println(&format!("files: {} wants an account; netlogon is task {task}", self.path));
                self.net_login = Some(NetLogin { task, path: self.path.clone(), closed: false });
                return;
            }
        }
        let why = match code {
            user_abi::ERR_PERMISSION => String::from("нужна учётная запись"),
            user_abi::ERR_NOT_FOUND => String::from("компьютер или папка не найдены"),
            user_abi::ERR_NO_NETWORK => String::from("нет сети"),
            other => error_text(other),
        };
        self.error = Some(format!("Нет доступа к {}: {why}", net_title(&self.path)));
    }

    /// Loading, the password window, or an empty «Сеть» — over the list.
    /// `true` — drawn (the list is not).
    pub(super) fn draw_net_state(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) -> bool {
        if !is_net(&self.path) || self.error.is_some() {
            return false;
        }
        let (title, hint) = if self.net_login.as_ref().is_some_and(|login| !login.closed) {
            (format!("Вход в {}…", net_title(&self.path)), String::from("Введите имя и пароль в окне «Безопасность FossAi»"))
        } else if self.net_load.is_some() {
            if self.path == sysconf::winpath::NET {
                (String::from("Поиск компьютеров в сети…"), String::from("Компьютеры отвечают по SMB, NetBIOS, mDNS и WS-Discovery"))
            } else {
                (format!("Загрузка {}…", net_title(&self.path)), String::from("Подключение к компьютеру может занять несколько секунд"))
            }
        } else if self.rows.is_empty() && self.path == sysconf::winpath::NET {
            (String::from("Компьютеры в сети не найдены"), String::from("Наберите в адресе \\\\имя или \\\\адрес компьютера"))
        } else {
            return false;
        };
        let p = ctx.palette;
        let area = plan.list.union(&plan.header);
        let side = ctx.px(64);
        let cx = area.x + area.w as i32 / 2;
        let top = (area.y + area.h as i32 / 3 - side as i32).max(area.y + ctx.px(8) as i32);
        let icon = Rect::new(cx - side as i32 / 2, top, side, side);
        draw_design_icon(&self.icons, s, icon, "network");
        let y = icon.bottom() + ctx.px(14) as i32;
        let room = area.w.saturating_sub(ctx.px(32));
        let tw = (ctx.face(Role::Strong).width(&title)).min(room) as i32;
        paint::text_clipped(ctx, s, Role::Strong, cx - tw / 2, y, room, &title, p.ink2);
        let hw = (ctx.face(Role::Body).width(&hint)).min(room) as i32;
        paint::text_clipped(ctx, s, Role::Body, cx - hw / 2, y + ctx.px(26) as i32, room, &hint, p.ink4);
        true
    }

    /// Picture and type of a row: computers in `/net`, shared folders of a
    /// computer, then as everywhere.
    pub(super) fn row_art(&self, row: &Row) -> (&'static str, &'static str) {
        if row.is_volume() {
            return (volume_art(&row.name), if row.name == sysconf::winpath::NET { "Сеть" } else { "Диск" });
        }
        match net_level(&self.path) {
            Some(0) => ("place-this-pc", "Компьютер"),
            Some(1) => ("place-network-folder", "Общая папка"),
            _ => file_kind(&row.name, row.directory),
        }
    }
}

/// Picture of a volume tile («Этот компьютер»); «Сеть» — the network.
pub(super) fn volume_art(point: &str) -> &'static str {
    if point == "/" {
        "place-system-disk"
    } else if point.starts_with("/usb") {
        "place-usb-stick"
    } else if point == sysconf::winpath::NET {
        "network"
    } else {
        "place-local-disk"
    }
}
