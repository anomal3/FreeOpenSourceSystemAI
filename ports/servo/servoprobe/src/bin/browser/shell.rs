//! Состояние браузера: вкладки, события Servo, действия пользователя,
//! профиль, пароли, синхронизация. Рисует всё это `ui.rs`.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use browser_sync::json;
use browser_sync::remote::{Account, Provider, SyncPoint, oauth};
use servo::{
    AuthenticationRequest, ClipboardDelegate, ColorPicker, ConsoleLogLevel, ContextMenu, CreateNewWebViewRequest,
    EmbedderControl, EmbedderControlId, FilePicker, LoadStatus, NavigationRequest, RenderingContext, SelectElement,
    Servo, SimpleDialog, StringRequest, UserContentManager, WebView, WebViewBuilder, WebViewDelegate, WebViewId,
};

use crate::pages::{self, PageData};
use crate::profile::{self, Look, Profile, now_ms};
use crate::swgl_ctx::SwglRenderingContext;
use crate::syncer::{Done, Job, Syncer};
use crate::vault::{self, Login, Vault};
use crate::window::{self, Window};

// ── Геометрия ───────────────────────────────────────────────────────────────

/// Полоса вкладок сверху (если вкладки не сбоку).
pub const TABSTRIP_H: u32 = 36;
pub const TOOLBAR_H: u32 = 44;
pub const TOOLBAR_H_COMPACT: u32 = 38;

/// Где страница: левый верхний угол её прямоугольника в окне.
pub fn page_origin(look: &Look) -> (u32, u32) {
    let bar = if look.compact { TOOLBAR_H_COMPACT } else { TOOLBAR_H };
    if look.side_tabs { (look.sidebar_width, bar) } else { (0, TABSTRIP_H + bar) }
}

pub fn page_size(look: &Look, w: u32, h: u32) -> dpi::PhysicalSize<u32> {
    let (x, y) = page_origin(look);
    dpi::PhysicalSize::new(w.saturating_sub(x).max(1), h.saturating_sub(y).max(1))
}

// ── События Servo → главный цикл ────────────────────────────────────────────

pub enum Note {
    Frame(WebViewId),
    Title(WebViewId, Option<String>),
    Url(WebViewId, url::Url),
    Load(WebViewId, LoadStatus),
    Status(WebViewId, Option<String>),
    History,
    Opened(WebView),
    Crashed(WebViewId, String),
    Control(WebViewId, EmbedderControl),
    HideControl(EmbedderControlId),
    Auth(WebViewId, AuthenticationRequest),
    /// Сообщение пользовательского скрипта OBSOS (`__obsos__:{…}`).
    Script(WebViewId, String),
    /// Команда со страницы `obsos://action/…` или подмена адреса.
    Action(Action),
    /// Заблокирован запрос рекламы/трекера на странице.
    Blocked(WebViewId),
}

pub struct Delegate {
    pub notes: RefCell<Vec<Note>>,
    pub context: Rc<SwglRenderingContext>,
    /// Запасной буфер обмена (если ядро без SYS_CLIPBOARD); обычно — общий
    /// буфер системы (`window::clipboard_*`).
    pub clipboard: Rc<RefCell<String>>,
    pub content: Rc<UserContentManager>,
    /// Форма указателя, которую просит страница (над ссылкой — рука).
    pub page_cursor: std::cell::Cell<window::CursorShape>,
    pub blocker: Rc<RefCell<crate::blocker::Blocker>>,
    pub downloads: Rc<crate::downloads::Downloads>,
    /// Консоль и сеть для инструментов разработчика (F12).
    pub devlog: RefCell<crate::devtools::DevLog>,
}

impl Delegate {
    fn note(&self, note: Note) {
        self.notes.borrow_mut().push(note);
    }
}

const SCRIPT_PREFIX: &str = "__obsos__:";

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, webview: WebView) {
        self.note(Note::Frame(webview.id()));
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        println!("browser: load status {status:?}");
        self.note(Note::Load(webview.id(), status));
    }

    fn notify_page_title_changed(&self, webview: WebView, title: Option<String>) {
        println!("browser: title {title:?}");
        self.note(Note::Title(webview.id(), title));
    }

    fn notify_url_changed(&self, webview: WebView, url: url::Url) {
        let shown = url.as_str();
        println!("browser: url {}", if shown.starts_with("data:") { "data:…" } else { shown });
        self.note(Note::Url(webview.id(), url));
    }

    fn notify_status_text_changed(&self, webview: WebView, status: Option<String>) {
        self.note(Note::Status(webview.id(), status));
    }

    fn notify_history_changed(&self, _webview: WebView, _entries: Vec<url::Url>, _current: usize) {
        self.note(Note::History);
    }

    fn notify_crashed(&self, webview: WebView, reason: String, _backtrace: Option<String>) {
        println!("browser: page crashed: {reason}");
        self.note(Note::Crashed(webview.id(), reason));
    }

    fn request_create_new(&self, parent: WebView, request: CreateNewWebViewRequest) {
        let webview = request
            .builder(self.context.clone() as Rc<dyn RenderingContext>)
            .delegate(parent.delegate())
            .clipboard_delegate(parent.clipboard_delegate())
            .user_content_manager(self.content.clone())
            .build();
        self.note(Note::Opened(webview));
    }

    fn request_navigation(&self, _webview: WebView, request: NavigationRequest) {
        let url = request.url.clone();
        if url.scheme() == "obsos" && url.host_str() == Some("action") {
            let target = url.query_pairs().find(|(k, _)| k == "url").map(|(_, v)| v.into_owned()).unwrap_or_default();
            let action = match url.path() {
                "/chromium" => Some(Action::Chromium(target)),
                "/force" => Some(Action::OpenUrl(target)),
                _ => None,
            };
            request.deny();
            if let Some(action) = action {
                self.note(Note::Action(action));
            }
            return;
        }
        if crate::downloads::looks_like_file(&url) {
            request.deny();
            self.note(Note::Action(Action::Download(url.to_string())));
            return;
        }
        if url.scheme() == "file" && pages::viewable_path(url.path()) {
            request.deny();
            self.note(Note::Action(Action::OpenUrl(pages::view_url(url.path()).to_string())));
            return;
        }
        request.allow();
    }

    fn notify_cursor_changed(&self, _webview: WebView, cursor: servo::Cursor) {
        use servo::Cursor as C;
        use window::CursorShape as S;
        self.page_cursor.set(match cursor {
            C::Pointer => S::Hand,
            C::Text | C::VerticalText => S::Text,
            C::Move | C::Grab | C::Grabbing | C::AllScroll => S::Move,
            C::NotAllowed | C::NoDrop => S::NotAllowed,
            C::EResize | C::WResize | C::EwResize | C::ColResize => S::ResizeEw,
            C::NResize | C::SResize | C::NsResize | C::RowResize => S::ResizeNs,
            _ => S::Arrow,
        });
    }

    /// Блокировка рекламы и трекеров: запрос к рекламному домену отменяется
    /// до сети. Сам документ страницы не трогаем.
    fn load_web_resource(&self, webview: WebView, load: servo::WebResourceLoad) {
        let request = load.request();
        let host = request.url.host_str().map(str::to_string).unwrap_or_default();
        let site = webview.url().and_then(|u| u.host_str().map(str::to_string));
        let blocked = !request.is_for_main_frame && !host.is_empty() && self.blocker.borrow().blocks(&host, site.as_deref());
        self.devlog.borrow_mut().request(crate::devtools::NetEntry {
            webview: webview.id(),
            method: request.method.to_string(),
            url: request.url.as_str().to_string(),
            kind: format!("{:?}", request.destination),
            main: request.is_for_main_frame,
            blocked,
            at: Instant::now(),
        });
        if blocked {
            let url = request.url.as_url().clone();
            let mut blocker = self.blocker.borrow_mut();
            blocker.blocked_total += 1;
            if blocker.blocked_total <= 3 || blocker.blocked_total % 50 == 0 {
                println!("browser: blocked {host} ({} total)", blocker.blocked_total);
            }
            drop(blocker);
            self.note(Note::Blocked(webview.id()));
            load.intercept(servo::WebResourceResponse::new(url)).cancel();
        }
    }

    fn request_authentication(&self, webview: WebView, request: AuthenticationRequest) {
        self.note(Note::Auth(webview.id(), request));
    }

    fn show_embedder_control(&self, webview: WebView, control: EmbedderControl) {
        self.note(Note::Control(webview.id(), control));
    }

    fn hide_embedder_control(&self, _webview: WebView, id: EmbedderControlId) {
        self.note(Note::HideControl(id));
    }

    fn show_console_message(&self, webview: WebView, level: ConsoleLogLevel, message: String) {
        if let Some(payload) = message.strip_prefix(SCRIPT_PREFIX) {
            self.note(Note::Script(webview.id(), payload.to_string()));
            return;
        }
        let level = format!("{level:?}");
        // Ошибки страниц — и в журнал системы: по нему видно, на чём сайт
        // сломался, даже без открытой консоли.
        if level.contains("Error") || level.contains("Warn") {
            let short: String = message.chars().take(400).collect();
            println!("browser: console {level}: {short}");
        }
        self.devlog.borrow_mut().console(Some(webview.id()), &level, message);
    }
}

impl ClipboardDelegate for Delegate {
    fn clear(&self, _webview: WebView) {
        self.clipboard.borrow_mut().clear();
    }

    fn get_text(&self, _webview: WebView, request: StringRequest) {
        // Буфер обмена системы (общий с терминалом и Блокнотом); старое ядро
        // без него — свой.
        request.success(window::clipboard_get().unwrap_or_else(|| self.clipboard.borrow().clone()));
    }

    fn set_text(&self, _webview: WebView, text: String) {
        window::clipboard_set(&text);
        *self.clipboard.borrow_mut() = text;
    }
}

/// Пользовательский скрипт OBSOS в каждой странице: перехват отправки формы
/// с паролем (Servo не даёт оболочке события «форма отправлена»), сообщение
/// о поле пароля (для подстановки) и о том, что сайт полез в `MediaSource`
/// (потокового видео в Servo нет) — сообщения уходят в консоль, их ловит
/// `show_console_message`.
pub const PAGE_SCRIPT: &str = r#"(function(){
if (window.__obsos) return; window.__obsos = 1;
function send(o){ try { console.log('__obsos__:' + JSON.stringify(o)); } catch(e){} }
// Servo отдаёт потоковой компиляции wasm тело ответа ещё сжатым (gzip от
// сервера) — «failed to match magic number» у .NET/Blazor. arrayBuffer() даёт
// распакованное: компилируем из него.
try {
  if (window.WebAssembly) {
    var W = WebAssembly, body = function(src){ return Promise.resolve(src).then(function(r){ return r.arrayBuffer(); }); };
    W.instantiateStreaming = function(src, imports){ return body(src).then(function(b){ return W.instantiate(b, imports); }); };
    W.compileStreaming = function(src){ return body(src).then(function(b){ return W.compile(b); }); };
  }
} catch(e) {}
window.addEventListener('error', function(e){ if (!(e instanceof ErrorEvent)) return; try { console.error('Uncaught ' + (e.message || e) + (e.filename ? ' @ ' + e.filename + ':' + e.lineno + ':' + e.colno : '')); } catch(x){} }, true);
window.addEventListener('unhandledrejection', function(e){ try { var r = e.reason; console.error('Unhandled rejection: ' + (r && (r.stack || r.message) || r)); } catch(x){} });
try {
  if (!('MediaSource' in window)) {
    var told = false;
    Object.defineProperty(window, 'MediaSource', { configurable: true, get: function(){
      if (!told) { told = true; send({t:'unsupported', what:'MediaSource'}); } return undefined; } });
  }
} catch(e) {}
var last = '';
function grab(form){
  if (!form || !form.querySelector) return;
  var pw = form.querySelector('input[type=password]');
  if (!pw || !pw.value) return;
  var user = null, ins = form.querySelectorAll('input');
  for (var i = 0; i < ins.length; i++) {
    if (ins[i] === pw) break;
    var t = (ins[i].type || 'text').toLowerCase();
    if (t == 'text' || t == 'email' || t == 'tel' || t == '') user = ins[i];
  }
  var key = (user ? user.value : '') + '\u0000' + pw.value;
  if (key == last) return; last = key;
  send({t:'login', user: user ? user.value : '', pass: pw.value});
}
document.addEventListener('submit', function(e){ grab(e.target); }, true);
document.addEventListener('click', function(e){
  var b = e.target; while (b && b.tagName != 'BUTTON' && !(b.tagName == 'INPUT' && b.type == 'submit')) b = b.parentElement;
  if (b && b.form) grab(b.form);
}, true);
document.addEventListener('keydown', function(e){ if (e.key == 'Enter' && e.target && e.target.form) grab(e.target.form); }, true);
document.addEventListener('click', function(e){
  var a = e.target; while (a && a.tagName != 'A') a = a.parentElement;
  if (a && a.hasAttribute('download') && a.href && /^https?:/.test(a.href)) { e.preventDefault(); send({t:'download', url: a.href}); }
}, true);
function probe(){ if (document.querySelector('input[type=password]')) send({t:'pwfield'}); }
if (document.readyState == 'complete') probe(); else window.addEventListener('load', probe);
})();"#;

/// Скрипт подстановки входа (значения — строки JSON).
pub fn fill_script(user: &str, pass: &str) -> String {
    let quote = |s: &str| {
        let mut out = String::new();
        json::write_str(s, &mut out);
        out
    };
    format!(
        "(function(u,p){{var pw=document.querySelector('input[type=password]');if(!pw)return false;var f=pw.form||document;\
var ins=f.querySelectorAll('input'),user=null;for(var i=0;i<ins.length;i++){{if(ins[i]===pw)break;var t=(ins[i].type||'text').toLowerCase();\
if(t=='text'||t=='email'||t=='tel'||t=='')user=ins[i];}}function set(el,v){{el.focus();el.value=v;\
el.dispatchEvent(new Event('input',{{bubbles:true}}));el.dispatchEvent(new Event('change',{{bubbles:true}}));}}\
if(user&&u)set(user,u);set(pw,p);return true;}})({},{})",
        quote(user),
        quote(pass)
    )
}

// ── Оболочка ────────────────────────────────────────────────────────────────

pub struct Tab {
    pub webview: WebView,
    pub title: String,
    /// Адрес для показа (`obsos://newtab` — пусто).
    pub url: String,
    pub real_url: Option<url::Url>,
    pub loading: bool,
    pub status: Option<String>,
    pub zoom: f32,
    /// Сколько запросов рекламы/трекеров заблокировано на этой странице.
    pub blocked: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    NewTab,
    NewTabUrl(String),
    Activate(usize),
    Close(usize),
    Back,
    Forward,
    Reload,
    Go(String),
    OpenUrl(String),
    FocusAddress,
    ToggleBookmark,
    Chromium(String),
    Zoom(f32),
    Panel(Panel),
    SyncNow(Option<String>),
    /// Поиск на странице: открыть строку (Ctrl+F), следующее/предыдущее, закрыть.
    FindOpen,
    Find { back: bool },
    /// Скачать адрес в «Загрузки».
    Download(String),
    /// Блокировка рекламы на текущем сайте: выключить/включить (щит).
    ToggleBlockSite,
    /// Режим чтения: только текст статьи.
    Reader,
    /// Новое окно инкогнито (Ctrl+Shift+N): профиль во временном каталоге.
    PrivateWindow,
    /// Вернуть последнюю закрытую вкладку (Ctrl+Shift+T).
    Reopen,
    /// Домашняя страница в текущей вкладке (Alt+Home).
    Home,
    FindClose,
    Quit,
}

/// Состояние генератора паролей, проверки утечек, импорта и скачивания
/// списков фильтров (ответы фоновых потоков — в `Arc<Mutex>`).
pub struct ToolsUi {
    pub gen_len: usize,
    pub gen_symbols: bool,
    pub generated: String,
    pub import_path: String,
    /// Запись пароля → «не найден в утечках» / «найден N раз».
    pub leaks: Arc<Mutex<std::collections::BTreeMap<String, String>>>,
    /// Скачанный список EasyList (или ошибка), пока главный поток не забрал.
    pub filters: Arc<Mutex<Option<Result<String, String>>>>,
    pub filters_busy: bool,
}

impl Default for ToolsUi {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| String::from("/home"));
        ToolsUi {
            gen_len: 20,
            gen_symbols: true,
            generated: String::new(),
            import_path: format!("{home}/Downloads/"),
            leaks: Arc::default(),
            filters: Arc::default(),
            filters_busy: false,
        }
    }
}

/// Строка поиска на странице. В Servo своего поиска нет — ищет скрипт
/// (`find_script`): подсвечивает совпадения `<mark>` и прокручивает к текущему.
#[derive(Default)]
pub struct FindBar {
    pub open: bool,
    pub text: String,
    pub focus: bool,
    /// «3/12» — ответ скрипта (пишет обратный вызов `evaluate_javascript`).
    pub status: Rc<RefCell<String>>,
}

/// Скрипт поиска: `q` — строка (пусто — убрать подсветку), `back` — назад.
pub fn find_script(q: &str, back: bool) -> String {
    let mut quoted = String::new();
    json::write_str(q, &mut quoted);
    format!(
        r#"(function(q,back){{var s=window.__obsosFind;
if(!s||s.q!==q){{if(s){{s.marks.forEach(function(m){{var p=m.parentNode;if(p){{p.replaceChild(document.createTextNode(m.textContent),m);p.normalize();}}}});}}
s={{q:q,marks:[],i:-1}};window.__obsosFind=s;
if(q&&document.body){{var lq=q.toLowerCase(),w=document.createTreeWalker(document.body,NodeFilter.SHOW_TEXT,null),nodes=[],n;
while((n=w.nextNode())){{var t=n.parentNode&&n.parentNode.nodeName;if(t!=='SCRIPT'&&t!=='STYLE'&&t!=='TEXTAREA'&&n.nodeValue.toLowerCase().indexOf(lq)>=0)nodes.push(n);}}
nodes.forEach(function(n){{var v=n.nodeValue,lv=v.toLowerCase(),pos=0,i,f=document.createDocumentFragment();
while((i=lv.indexOf(lq,pos))>=0){{f.appendChild(document.createTextNode(v.slice(pos,i)));var m=document.createElement('mark');
m.style.background='#ffd54a';m.style.color='#000';m.textContent=v.slice(i,i+q.length);f.appendChild(m);s.marks.push(m);pos=i+q.length;}}
f.appendChild(document.createTextNode(v.slice(pos)));n.parentNode.replaceChild(f,n);}});}}}}
if(!s.marks.length)return q?'0/0':'';
if(s.i>=0)s.marks[s.i].style.background='#ffd54a';
s.i=back?(s.i<=0?s.marks.length-1:s.i-1):(s.i+1)%s.marks.length;
var m=s.marks[s.i];m.style.background='#ff8a3d';m.scrollIntoView({{block:'center'}});return (s.i+1)+'/'+s.marks.length;}})({quoted},{back})"#
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    Menu,
    Bookmarks,
    History,
    Passwords,
    RemoteTabs,
    Sync,
    Looks,
    Settings,
    Scripts,
    Profiles,
    Downloads,
    DevTools,
}

pub enum Modal {
    Dialog(SimpleDialog, String),
    Auth { request: AuthenticationRequest, host: String, user: String, pass: String, remember: bool },
    Select(SelectElement, egui::Pos2),
    Context(ContextMenu, egui::Pos2),
    Color(ColorPicker, [u8; 3]),
    File(FilePicker, String),
    Master { text: String, error: String },
    Rollback(String),
}

impl Modal {
    fn control_id(&self) -> Option<EmbedderControlId> {
        match self {
            Modal::Select(c, _) => Some(c.id()),
            Modal::Context(c, _) => Some(c.id()),
            Modal::Color(c, _) => Some(c.id()),
            Modal::File(c, _) => Some(c.id()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Infobar {
    SaveLogin(Login),
    ChooseLogin(String),
    Unsupported { url: String, what: String },
}

/// Вход через браузер (Device Flow), пока ждём подтверждения.
pub struct LoginFlow {
    pub code: oauth::DeviceCode,
    pub next_poll: Instant,
    pub interval: Duration,
    pub until: Instant,
}

pub struct SyncUi {
    /// Редактируемая учётная запись (форма в панели).
    pub draft: Account,
    pub label: String,
    pub points: Vec<SyncPoint>,
    pub status: String,
    pub flow: Option<LoginFlow>,
    pub sync_password: String,
    pub next_auto: Instant,
    pub last_report: String,
}

pub struct Shell {
    pub servo: Servo,
    pub context: Rc<SwglRenderingContext>,
    pub delegate: Rc<Delegate>,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub address: String,
    pub focus_address: bool,
    pub opened: usize,
    pub profile: Profile,
    pub vault: Option<Vault>,
    pub page_data: Arc<Mutex<PageData>>,
    pub panel: Option<Panel>,
    pub modal: Option<Modal>,
    pub infobars: Vec<Infobar>,
    pub toasts: Vec<(String, Instant)>,
    pub filter: String,
    pub syncer: Syncer,
    pub sync: SyncUi,
    pub show_passwords: BTreeSet<String>,
    pub new_profile: String,
    /// Сколько раз уже подставили сохранённый HTTP-вход для адреса (чтобы не
    /// зациклиться на неверном).
    pub auth_tries: Vec<String>,
    pub find: FindBar,
    pub tools: ToolsUi,
    pub dev: crate::devtools::DevUi,
    /// Закрытые вкладки — для Ctrl+Shift+T (последняя — в конце).
    pub closed: Vec<String>,
    pub quit: bool,
}

pub fn shown_url(url: &url::Url) -> String {
    match url.scheme() {
        "data" => String::new(),
        "obsos" if url.host_str() == Some("newtab") => String::new(),
        "obsos" if url.host_str() == Some("view") => {
            url.query_pairs().find(|(k, _)| k == "path").map(|(_, v)| v.into_owned()).unwrap_or_default()
        }
        _ => url.as_str().to_string(),
    }
}

impl Shell {
    pub fn look(&self) -> &Look {
        &self.profile.settings.look
    }

    pub fn active_view(&self) -> Option<&WebView> {
        self.tabs.get(self.active).map(|t| &t.webview)
    }

    pub fn toast(&mut self, text: impl Into<String>) {
        let text = text.into();
        println!("browser: toast {text}");
        self.toasts.push((text, Instant::now() + Duration::from_secs(5)));
    }

    /// Что набрали в адресной строке → адрес.
    pub fn url_from_input(&self, text: &str) -> Option<url::Url> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        if text.starts_with('/') {
            return Some(if pages::viewable_path(text) { pages::view_url(text) } else { url::Url::parse(&format!("file://{text}")).ok()? });
        }
        if text.contains("://") || text.starts_with("data:") || text.starts_with("about:") || text.starts_with("obsos:") {
            return url::Url::parse(text).ok();
        }
        if !text.contains(' ') && (text.contains('.') || text.starts_with("localhost")) {
            return url::Url::parse(&format!("https://{text}")).ok();
        }
        Some(self.profile.settings.search.url(text))
    }

    pub fn home_url(&self) -> url::Url {
        url::Url::parse(&self.profile.settings.home).unwrap_or_else(|_| url::Url::parse("obsos://newtab").expect("newtab"))
    }

    pub fn update_page_data(&self) {
        let look = self.look();
        let search = self.profile.settings.search;
        let probe = search.url("");
        let field = probe.query_pairs().next().map(|(k, _)| k.into_owned()).unwrap_or_else(|| "q".into());
        let mut action = probe.clone();
        action.set_query(None);
        let data = PageData {
            bookmarks: self.profile.bookmarks.iter().map(|b| (b.url.clone(), b.title.clone())).collect(),
            search_action: action.to_string(),
            search_field: field,
            search_title: search.title().to_string(),
            dark: look.dark,
            accent: look.accent.hex(),
            accent2: look.accent2.hex(),
            chromium: window::chromium_installed(),
            notice_path: String::from("/opt/obsos/share/NOTICE.md"),
        };
        if let Ok(mut shared) = self.page_data.lock() {
            if *shared == data {
                return;
            }
            *shared = data;
        }
        // Открытые «Новые вкладки» показывают старые плитки — перерисовать.
        for tab in &self.tabs {
            if tab.real_url.as_ref().is_some_and(|u| u.as_str().starts_with("obsos://newtab")) {
                tab.webview.reload();
            }
        }
    }

    // ── Вкладки ─────────────────────────────────────────────────────────

    fn open_tab(&mut self, webview: WebView, window: &Window) {
        self.opened += 1;
        webview.resize(page_size(self.look(), window.width.get(), window.height.get()));
        self.tabs.push(Tab {
            webview,
            title: String::from("Новая вкладка"),
            url: String::new(),
            real_url: None,
            loading: true,
            status: None,
            zoom: 1.0,
            blocked: 0,
        });
        let index = self.tabs.len() - 1;
        println!("browser: tab {} opened ({} open)", self.opened, self.tabs.len());
        self.activate(index, window);
    }

    pub fn new_tab(&mut self, url: url::Url, window: &Window) {
        let webview = WebViewBuilder::new(&self.servo, self.context.clone() as Rc<dyn RenderingContext>)
            .delegate(self.delegate.clone())
            .clipboard_delegate(self.delegate.clone())
            .user_content_manager(self.delegate.content.clone())
            .url(url)
            .build();
        self.open_tab(webview, window);
    }

    pub fn activate(&mut self, index: usize, window: &Window) {
        if index >= self.tabs.len() {
            return;
        }
        for (i, tab) in self.tabs.iter().enumerate() {
            if i != index {
                tab.webview.hide();
            }
        }
        self.active = index;
        let tab = &self.tabs[index];
        tab.webview.show();
        tab.webview.set_focused(true);
        tab.webview.paint();
        self.context.present();
        self.address = tab.url.clone();
        window.set_title(&format!("{} — {}", if self.profile.private { "OBSOS (инкогнито)" } else { "OBSOS" }, tab.title));
        self.infobars.clear();
        println!("browser: tab {} active", index + 1);
    }

    /// Закрыть вкладку; `false` — вкладок не осталось.
    pub fn close_tab(&mut self, index: usize, window: &Window) -> bool {
        if index >= self.tabs.len() {
            return true;
        }
        let tab = self.tabs.remove(index);
        if let Some(url) = tab.real_url {
            self.closed.push(url.to_string());
            if self.closed.len() > 25 {
                self.closed.remove(0);
            }
        }
        println!("browser: tab {} closed ({} open)", index + 1, self.tabs.len());
        if self.tabs.is_empty() {
            return false;
        }
        let next = if self.active > index || self.active >= self.tabs.len() { self.active.saturating_sub(1) } else { self.active };
        self.activate(next.min(self.tabs.len() - 1), window);
        true
    }

    fn tab_index(&self, id: WebViewId) -> Option<usize> {
        self.tabs.iter().position(|t| t.webview.id() == id)
    }

    pub fn resize_all(&self, window: &Window) {
        let size = page_size(self.look(), window.width.get(), window.height.get());
        // Холст swgl Servo меняет сам (`resize_rendering_context`) и, если
        // холст уже нужного размера, пропускает перестройку WebRender, —
        // поэтому трогать его здесь нельзя.
        for tab in &self.tabs {
            tab.webview.resize(size);
        }
    }

    pub fn load(&mut self, url: url::Url) {
        println!("browser: go {}", url.as_str());
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.url = shown_url(&url);
            tab.loading = true;
            tab.webview.load(url);
            tab.webview.set_focused(true);
        }
        self.infobars.clear();
    }

    // ── События Servo ───────────────────────────────────────────────────

    /// Разобрать накопленные события. Возвращает: менялась ли оболочка.
    pub fn apply_notes(&mut self, window: &Window) -> bool {
        let notes = std::mem::take(&mut *self.delegate.notes.borrow_mut());
        let changed = !notes.is_empty();
        for note in notes {
            match note {
                Note::Frame(id) => {
                    if self.active_view().is_some_and(|w| w.id() == id) {
                        let webview = self.tabs[self.active].webview.clone();
                        webview.paint();
                        self.context.present();
                    }
                }
                Note::Title(id, title) => {
                    let active = self.active;
                    if let Some(i) = self.tab_index(id) {
                        let tab = &mut self.tabs[i];
                        tab.title = title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).unwrap_or_else(|| {
                            if tab.url.is_empty() { String::from("Новая вкладка") } else { tab.url.clone() }
                        });
                        if i == active {
                            window.set_title(&format!("{} — {}", if self.profile.private { "OBSOS (инкогнито)" } else { "OBSOS" }, tab.title));
                        }
                    }
                }
                Note::Url(id, url) => {
                    let focused = self.focus_address;
                    let active = self.active;
                    let shown = shown_url(&url);
                    if let Some(i) = self.tab_index(id) {
                        self.tabs[i].url = shown.clone();
                        self.tabs[i].real_url = Some(url);
                        if i == active && !focused {
                            self.address = shown;
                        }
                    }
                }
                Note::Load(id, status) => {
                    if let Some(i) = self.tab_index(id) {
                        self.tabs[i].loading = status != LoadStatus::Complete;
                        if status == LoadStatus::Started {
                            self.tabs[i].blocked = 0;
                        }
                        if status == LoadStatus::Complete {
                            let (url, title) = (self.tabs[i].real_url.clone(), self.tabs[i].title.clone());
                            if let Some(url) = url {
                                self.profile.visit(url.as_str(), &title);
                                if url.scheme().starts_with("http") && self.tabs[i].title == self.tabs[i].url {
                                    self.check_unknown_type(i);
                                }
                            }
                        }
                    }
                }
                Note::Status(id, status) => {
                    if let Some(i) = self.tab_index(id) {
                        self.tabs[i].status = status;
                    }
                }
                Note::History => {}
                Note::Opened(webview) => self.open_tab(webview, window),
                Note::Crashed(id, reason) => {
                    if let Some(i) = self.tab_index(id) {
                        let url = self.tabs[i].real_url.as_ref().map(|u| u.to_string()).unwrap_or_default();
                        self.tabs[i].webview.load(pages::crashed_url(&url, &reason));
                    }
                }
                Note::Control(id, control) => self.show_control(id, control),
                Note::HideControl(id) => {
                    if self.modal.as_ref().and_then(Modal::control_id) == Some(id) {
                        self.modal = None;
                    }
                }
                Note::Auth(id, request) => self.auth_request(id, request),
                Note::Script(id, payload) => self.script_message(id, &payload),
                Note::Action(action) => {
                    self.run(action, window);
                }
                Note::Blocked(id) => {
                    if let Some(i) = self.tab_index(id) {
                        self.tabs[i].blocked += 1;
                    }
                }
            }
        }
        changed
    }

    /// Servo показал свою «Unknown content type»: страница без заголовка.
    fn check_unknown_type(&mut self, index: usize) {
        let delegate = self.delegate.clone();
        let id = self.tabs[index].webview.id();
        self.tabs[index].webview.evaluate_javascript(
            "document.body && document.body.innerText.indexOf('Unknown content type') == 0 ? document.body.innerText : ''",
            move |result| {
                if let Ok(servo::JSValue::String(text)) = result {
                    if !text.is_empty() {
                        let what = text.trim_start_matches("Unknown content type").trim().trim_matches(|c| c == '(' || c == ')' || c == '.').to_string();
                        println!("browser: unknown type {what} — download");
                        delegate.note(Note::Script(id, String::from("{\"t\":\"fetch\"}")));
                    }
                }
            },
        );
    }

    fn page_offset(&self) -> egui::Vec2 {
        let (x, y) = page_origin(self.look());
        egui::vec2(x as f32, y as f32)
    }

    fn show_control(&mut self, id: WebViewId, control: EmbedderControl) {
        if self.active_view().is_none_or(|w| w.id() != id) {
            return;
        }
        let offset = self.page_offset();
        self.modal = match control {
            EmbedderControl::SelectElement(select) => {
                let r = select.position();
                Some(Modal::Select(select, egui::pos2(r.min.x as f32, r.max.y as f32) + offset))
            }
            EmbedderControl::ContextMenu(menu) => {
                let r = menu.position();
                Some(Modal::Context(menu, egui::pos2(r.min.x as f32, r.min.y as f32) + offset))
            }
            EmbedderControl::ColorPicker(picker) => {
                let c = picker.current_color().map(|c| [c.red, c.green, c.blue]).unwrap_or([0, 0, 0]);
                Some(Modal::Color(picker, c))
            }
            EmbedderControl::FilePicker(picker) => {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/home".into());
                Some(Modal::File(picker, format!("{home}/")))
            }
            EmbedderControl::SimpleDialog(dialog) => {
                let value = match &dialog {
                    SimpleDialog::Prompt(p) => p.current_value().to_string(),
                    _ => String::new(),
                };
                Some(Modal::Dialog(dialog, value))
            }
            // Экранной клавиатуры на ПК нет — поле и так получает клавиши.
            EmbedderControl::InputMethod(_) => return,
        };
        println!("browser: control shown");
    }

    fn auth_request(&mut self, id: WebViewId, request: AuthenticationRequest) {
        let url = request.url().clone();
        let host = url.host_str().unwrap_or("").to_string();
        let key = url.as_str().to_string();
        if let Some(origin) = vault::origin_of(&url) {
            if let Some(login) = self.profile.logins_for(&origin).into_iter().next() {
                if !self.auth_tries.contains(&key) {
                    self.auth_tries.push(key);
                    println!("browser: http auth from vault for {host}");
                    request.authenticate(login.username, login.password);
                    return;
                }
            }
        }
        let _ = id;
        self.modal = Some(Modal::Auth { request, host, user: String::new(), pass: String::new(), remember: true });
    }

    fn origin_of_tab(&self, id: WebViewId) -> Option<String> {
        let i = self.tab_index(id)?;
        vault::origin_of(self.tabs[i].real_url.as_ref()?)
    }

    fn script_message(&mut self, id: WebViewId, payload: &str) {
        let Ok(v) = json::parse(payload) else { return };
        match v.str_field("t") {
            Some("login") => {
                if !self.profile.settings.save_passwords {
                    return;
                }
                let Some(origin) = self.origin_of_tab(id) else { return };
                let username = v.str_field("user").unwrap_or("").to_string();
                let password = v.str_field("pass").unwrap_or("").to_string();
                if password.is_empty() || self.never_save(&origin) {
                    return;
                }
                let known = self.profile.logins_for(&origin).into_iter().any(|l| l.username == username && l.password == password);
                if !known {
                    println!("browser: login form submitted on {origin}");
                    self.infobars.retain(|b| !matches!(b, Infobar::SaveLogin(_)));
                    self.infobars.push(Infobar::SaveLogin(Login { origin, username, password, updated_ms: now_ms() }));
                }
            }
            Some("pwfield") => {
                let Some(origin) = self.origin_of_tab(id) else { return };
                let logins = self.profile.logins_for(&origin);
                if !self.profile.settings.autofill || logins.is_empty() {
                    return;
                }
                if logins.len() == 1 {
                    self.fill(id, &logins[0]);
                } else if !self.infobars.iter().any(|b| matches!(b, Infobar::ChooseLogin(_))) {
                    self.infobars.push(Infobar::ChooseLogin(origin));
                }
            }
            // Servo не показывает такой тип — скачать, а вкладку вернуть назад.
            Some("fetch") => {
                let Some(i) = self.tab_index(id) else { return };
                let Some(url) = self.tabs[i].real_url.clone() else { return };
                self.download(url.as_str());
                let webview = self.tabs[i].webview.clone();
                if webview.can_go_back() {
                    webview.go_back(1);
                } else {
                    self.tabs[i].webview.load(url::Url::parse("obsos://newtab").expect("newtab"));
                }
            }
            // Ссылка `<a download>` на странице.
            Some("download") => {
                if let Some(url) = v.str_field("url") {
                    self.download(url);
                }
            }
            Some("unsupported") => {
                let what = v.str_field("what").unwrap_or("").to_string();
                let url = self.tab_index(id).and_then(|i| self.tabs[i].real_url.as_ref().map(|u| u.to_string())).unwrap_or_default();
                println!("browser: unsupported {what} on {url}");
                if !self.infobars.iter().any(|b| matches!(b, Infobar::Unsupported { .. })) {
                    self.infobars.push(Infobar::Unsupported { url, what });
                }
            }
            _ => {}
        }
    }

    pub fn fill(&mut self, id: WebViewId, login: &Login) {
        if let Some(i) = self.tab_index(id) {
            println!("browser: autofill {}", login.origin);
            self.tabs[i].webview.evaluate_javascript(fill_script(&login.username, &login.password), |_| {});
        }
    }

    pub fn never_save(&mut self, origin: &str) -> bool {
        self.profile.replica_view_has_never(origin)
    }

    // ── Действия ────────────────────────────────────────────────────────

    /// Выполнить действие. `false` — закрыть окно.
    pub fn run(&mut self, action: Action, window: &Window) -> bool {
        match action {
            Action::NewTab => {
                let url = url::Url::parse("obsos://newtab").expect("newtab");
                self.new_tab(url, window);
                self.address.clear();
                self.focus_address = true;
            }
            Action::NewTabUrl(text) => {
                if let Some(url) = self.url_from_input(&text) {
                    self.new_tab(url, window);
                }
            }
            Action::Activate(i) => self.activate(i, window),
            Action::Close(i) => return self.close_tab(i, window),
            Action::Back => {
                if let Some(w) = self.active_view().filter(|w| w.can_go_back()) {
                    w.go_back(1);
                    println!("browser: back");
                }
            }
            Action::Forward => {
                if let Some(w) = self.active_view().filter(|w| w.can_go_forward()) {
                    w.go_forward(1);
                    println!("browser: forward");
                }
            }
            Action::Reload => {
                if let Some(w) = self.active_view() {
                    w.reload();
                    println!("browser: reload");
                }
            }
            Action::Go(text) => match self.url_from_input(&text) {
                Some(url) => self.load(url),
                None => println!("browser: go ignored (empty)"),
            },
            Action::OpenUrl(text) => {
                if let Ok(url) = url::Url::parse(&text) {
                    self.load(url);
                }
            }
            Action::FocusAddress => self.focus_address = true,
            Action::ToggleBookmark => {
                let Some(tab) = self.tabs.get(self.active) else { return true };
                let Some(url) = tab.real_url.as_ref().map(|u| u.to_string()) else { return true };
                if url.starts_with("obsos://newtab") {
                    return true;
                }
                let title = tab.title.clone();
                if self.profile.is_bookmarked(&url) {
                    self.profile.remove_bookmark(&url);
                    self.toast("Закладка удалена");
                } else {
                    self.profile.add_bookmark(&url, &title);
                    println!("browser: bookmark added {url}");
                    self.toast("Добавлено в закладки");
                }
                self.update_page_data();
            }
            Action::Chromium(url) => {
                let url = if url.is_empty() { self.tabs.get(self.active).and_then(|t| t.real_url.as_ref()).map(|u| u.to_string()).unwrap_or_default() } else { url };
                if !window::chromium_installed() {
                    self.toast("Chromium не установлен — поставьте пакет chromium в «Пакетах»");
                } else {
                    match window::open_in_chromium(&url) {
                        Ok(task) => self.toast(format!("Открываю в Chromium (задача {task})")),
                        Err(e) => self.toast(format!("Chromium не запустился: {e}")),
                    }
                }
            }
            Action::Zoom(factor) => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.zoom = if factor == 0.0 { 1.0 } else { (tab.zoom * factor).clamp(0.3, 3.0) };
                    tab.webview.set_page_zoom(tab.zoom);
                }
            }
            Action::Panel(panel) => {
                self.panel = if self.panel == Some(panel) { None } else { Some(panel) };
                self.filter.clear();
                if panel == Panel::Sync && self.panel.is_some() {
                    self.load_points();
                }
            }
            Action::SyncNow(label) => self.sync_now(label),
            Action::Download(url) => self.download(&url),
            Action::ToggleBlockSite => {
                let Some(host) = self.tabs.get(self.active).and_then(|t| t.real_url.as_ref()?.host_str().map(str::to_string)) else { return true };
                let allowed = self.delegate.blocker.borrow_mut().toggle_site(&host);
                self.toast(if allowed { "Блокировка рекламы на этом сайте выключена" } else { "Блокировка рекламы на этом сайте включена" });
                if let Some(tab) = self.tabs.get(self.active) {
                    tab.webview.reload();
                }
            }
            Action::Reader => {
                if let Some(w) = self.active_view() {
                    println!("browser: reader mode");
                    w.evaluate_javascript(pages::READER_SCRIPT, |_| {});
                }
            }
            Action::PrivateWindow => {
                let _ = window::spawn("/opt/obsos/bin/obsos --private");
            }
            Action::Reopen => {
                if let Some(url) = self.closed.pop() {
                    if let Ok(url) = url::Url::parse(&url) {
                        self.new_tab(url, window);
                    }
                }
            }
            Action::Home => {
                let home = self.home_url();
                self.load(home);
            }
            Action::FindOpen => {
                self.find.open = true;
                self.find.focus = true;
            }
            Action::Find { back } => self.find_step(back),
            Action::FindClose => {
                self.find.open = false;
                self.find.status.borrow_mut().clear();
                if let Some(w) = self.active_view() {
                    w.evaluate_javascript(find_script("", false), |_| {});
                }
            }
            Action::Quit => return false,
        }
        true
    }

    pub fn download(&mut self, url: &str) {
        self.delegate.downloads.start(url);
        self.toast("Загрузка началась — Ctrl+J");
    }

    fn find_step(&mut self, back: bool) {
        let Some(w) = self.active_view() else { return };
        let status = self.find.status.clone();
        w.evaluate_javascript(find_script(&self.find.text, back), move |result| {
            let text = match result {
                Ok(servo::JSValue::String(t)) => t,
                _ => String::from("?"),
            };
            println!("browser: find {text}");
            *status.borrow_mut() = text;
        });
    }

    // ── Синхронизация ───────────────────────────────────────────────────

    pub fn account(&self) -> Option<Account> {
        self.vault.as_ref()?.account().cloned()
    }

    pub fn sync_now(&mut self, label: Option<String>) {
        let Some(account) = self.account() else {
            self.sync.status = String::from("Синхронизация не настроена");
            return;
        };
        let names: Vec<(String, String)> = self.tabs.iter().filter_map(|t| Some((t.real_url.as_ref()?.to_string(), t.title.clone()))).collect();
        self.profile.publish_tabs(&names);
        self.sync.status = String::from("Синхронизация…");
        println!("browser: sync start ({})", account.provider.title());
        self.syncer.send(Job::Sync {
            account,
            device: self.profile.settings.device.clone(),
            dir: self.profile.dir.clone(),
            point: label,
        });
    }

    pub fn load_points(&mut self) {
        if let Some(account) = self.account() {
            self.syncer.send(Job::Points { account });
        }
    }

    pub fn rollback(&mut self, point: String) {
        if let Some(account) = self.account() {
            self.sync.status = format!("Читаю точку {point}…");
            self.syncer.send(Job::FetchPoint { account, point });
        }
    }

    pub fn login_start(&mut self) {
        let account = self.sync.draft.clone();
        self.sync.status = String::from("Начинаю вход через браузер…");
        self.syncer.send(Job::LoginStart { account });
    }

    pub fn check_account(&mut self) {
        let account = self.sync.draft.clone();
        if let Err(e) = account.check() {
            self.sync.status = format!("Не хватает: {e}");
            return;
        }
        self.sync.status = String::from("Проверяю…");
        self.syncer.send(Job::Check { account });
    }

    /// Ответы потока синхронизации; плюс автосинхронизация и опрос входа.
    pub fn poll_sync(&mut self, window: &Window) -> bool {
        let mut changed = false;
        if let Some(flow) = &self.sync.flow {
            if Instant::now() >= flow.next_poll && self.syncer.busy == 0 {
                let device_code = flow.code.device_code.clone();
                if Instant::now() > flow.until {
                    self.sync.flow = None;
                    self.sync.status = String::from("Код входа истёк — начните заново");
                } else {
                    let account = self.sync.draft.clone();
                    self.syncer.send(Job::LoginPoll { account, device_code });
                    if let Some(f) = self.sync.flow.as_mut() {
                        f.next_poll = Instant::now() + f.interval;
                    }
                }
            }
        }
        let auto = self.profile.settings.sync_auto && self.account().is_some();
        if auto && Instant::now() >= self.sync.next_auto && self.syncer.busy == 0 {
            self.sync.next_auto = Instant::now() + Duration::from_secs(self.profile.settings.sync_every_min as u64 * 60);
            let label = self.profile.settings.sync_points_auto.then(String::new);
            self.sync_now(label);
        }
        while let Some(done) = self.syncer.poll() {
            changed = true;
            match done {
                Done::Synced(result, refreshed) => {
                    if let (Some(updated), Some(vault)) = (refreshed, self.vault.as_mut()) {
                        if let Some(i) = vault.active {
                            vault.accounts[i] = updated;
                            vault.save();
                        }
                    }
                    match result {
                        Ok(report) => {
                            self.profile.reload();
                            if let Some(v) = &self.vault {
                                let key = v.sync_key;
                                self.profile.reload_logins(&key);
                            }
                            self.update_page_data();
                            let point = report.point.as_ref().map(|p| format!(", точка {p}")).unwrap_or_default();
                            self.sync.last_report = format!(
                                "↑ {} ↓ {} · устройств: {}{point}",
                                report.pushed, report.pulled, report.devices
                            );
                            self.sync.status = format!("Синхронизировано: {}", self.sync.last_report);
                            println!("browser: sync ok pushed {} pulled {} devices {}{point}", report.pushed, report.pulled, report.devices);
                            if report.point.is_some() {
                                self.toast(format!("Точка синхронизации создана{point}"));
                                self.load_points();
                            }
                        }
                        Err(e) => {
                            self.sync.status = format!("Ошибка: {e}");
                            println!("browser: sync failed: {e}");
                        }
                    }
                }
                Done::Points(result) => match result {
                    Ok(points) => self.sync.points = points,
                    Err(e) => self.sync.status = format!("Точки: {e}"),
                },
                Done::Point(point, result) => match result {
                    Ok(then) => {
                        match browser_sync::sync::apply_rollback(&mut self.profile.replica, then, profile::SYNCED, now_ms()) {
                            Ok(r) => {
                                self.profile.reload();
                                if let Some(v) = &self.vault {
                                    let key = v.sync_key;
                                    self.profile.reload_logins(&key);
                                }
                                self.update_page_data();
                                self.sync.status = format!("Откат к {point}: возвращено {}, убрано {}", r.restored, r.removed);
                                println!("browser: rollback {point}: restored {} removed {}", r.restored, r.removed);
                                self.sync_now(None);
                            }
                            Err(e) => self.sync.status = format!("Откат: {e}"),
                        }
                    }
                    Err(e) => self.sync.status = format!("Откат: {e}"),
                },
                Done::LoginStarted(result) => match result {
                    Ok(code) => {
                        let interval = Duration::from_secs(code.interval.max(1));
                        self.sync.status = format!("Введите код {} на открывшейся странице", code.user_code);
                        if let Ok(url) = url::Url::parse(&code.verification_uri) {
                            self.new_tab(url, window);
                        }
                        self.sync.flow = Some(LoginFlow {
                            until: Instant::now() + Duration::from_secs(code.expires_in.max(60)),
                            next_poll: Instant::now() + interval,
                            interval,
                            code,
                        });
                    }
                    Err(e) => self.sync.status = format!("Вход: {e}"),
                },
                Done::LoginPolled(result) => match result {
                    Ok(oauth::Poll::Pending) => {}
                    Ok(oauth::Poll::SlowDown) => {
                        if let Some(f) = self.sync.flow.as_mut() {
                            f.interval += Duration::from_secs(5);
                        }
                    }
                    Ok(oauth::Poll::Done(tokens)) => {
                        self.sync.flow = None;
                        oauth::apply(&mut self.sync.draft, &tokens, now_ms());
                        self.sync.status = String::from("Вход выполнен — нажмите «Сохранить и включить»");
                    }
                    Ok(oauth::Poll::Failed(why)) => {
                        self.sync.flow = None;
                        self.sync.status = format!("Вход не удался: {why}");
                    }
                    Err(e) => self.sync.status = format!("Вход: {e}"),
                },
                Done::Checked(result) => match result {
                    Ok(()) => {
                        let draft = self.sync.draft.clone();
                        if let Some(vault) = self.vault.as_mut() {
                            match vault.accounts.iter().position(|a| a.provider == draft.provider) {
                                Some(i) => vault.accounts[i] = draft,
                                None => vault.accounts.push(draft),
                            }
                            vault.active = vault.accounts.iter().position(|a| a.provider == self.sync.draft.provider);
                            vault.save();
                        }
                        self.sync.status = String::from("Площадка готова, синхронизация включена");
                        println!("browser: sync account saved ({})", self.sync.draft.provider.title());
                        self.sync_now(None);
                    }
                    Err(e) => self.sync.status = format!("Проверка: {e}"),
                },
            }
        }
        changed
    }

    /// Выключить синхронизацию (учётная запись остаётся сохранённой).
    pub fn sync_off(&mut self) {
        if let Some(vault) = self.vault.as_mut() {
            vault.active = None;
            vault.save();
        }
        self.sync.status = String::from("Синхронизация выключена");
    }

    pub fn select_provider(&mut self, provider: Provider) {
        let saved = self.vault.as_ref().and_then(|v| v.accounts.iter().find(|a| a.provider == provider).cloned());
        self.sync.draft = saved.unwrap_or_else(|| Account::new(provider));
    }

    pub fn set_sync_password(&mut self) {
        let password = std::mem::take(&mut self.sync.sync_password);
        let Some(vault) = self.vault.as_mut() else { return };
        let old = vault.sync_key;
        vault.set_sync_password(&password);
        let new = vault.sync_key;
        self.profile.rekey_logins(&old, &new);
        self.toast(if password.is_empty() { "Пароли больше не расшифровываются на других устройствах" } else { "Пароль синхронизации задан — пароли перешифрованы" });
    }

    pub fn save_login(&mut self, login: Login) {
        if let Some(v) = &self.vault {
            let key = v.sync_key;
            self.profile.save_login(&key, login);
            self.toast("Пароль сохранён");
        }
    }

    /// Открыть хранилище мастер-паролем (окно спрашивает при запуске).
    pub fn unlock(&mut self, master: &str) {
        match Vault::open(&self.profile.dir, Some(master)) {
            Ok(vault) => {
                let key = vault.sync_key;
                self.profile.reload_logins(&key);
                self.vault = Some(vault);
                self.modal = None;
                println!("browser: vault unlocked");
            }
            Err(_) => self.modal = Some(Modal::Master { text: String::new(), error: String::from("Пароль не подошёл") }),
        }
    }

    pub fn session_urls(&self) -> Vec<String> {
        self.tabs.iter().filter_map(|t| t.real_url.as_ref().map(|u| u.to_string())).filter(|u| !u.starts_with("data:")).collect()
    }
}

impl Profile {
    /// Сайт в списке «никогда не сохранять пароль» (общие настройки).
    pub fn replica_view_has_never(&mut self, origin: &str) -> bool {
        self.replica.load(profile::SHARED).map(|v| v.state.get(&format!("never/{origin}")).is_some()).unwrap_or(false)
    }
}
