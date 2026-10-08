//! The background process that owns every tour, so the TUI and MCP servers share them.
//! Front-ends connect over localhost TCP and exchange one JSON message per line. The first
//! front-end to need it starts the daemon; it exits after a while without clients.
//! Each folder has one tour per agent. Tours are saved to disk and continued when their
//! folder is opened again.

use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::Agent;
use crate::engine::{Engine, State};
use crate::mcp;
use crate::tour::{Anchor, Tour};

/// How long the daemon stays up with no clients and no agent at work.
const IDLE_EXIT: Duration = Duration::from_secs(10 * 60);
/// The shortest gap between two state updates sent to one client.
const UPDATE_GAP: Duration = Duration::from_millis(30);

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    /// Must come first on every connection.
    Hello { token: String, build: String },
    /// Lists a folder's tours, answered with [`Reply::Tours`].
    List { root: PathBuf },
    /// Opens a folder's tour and sends its state now and whenever it changes.
    Open { root: PathBuf, agent: Agent, model: Option<String>, replan: bool },
    /// Asks a question at a stop, optionally about some lines of code.
    Ask { root: PathBuf, agent: Agent, stop: usize, question: String, code: Option<Anchor> },
    /// The person saw a stop, or one of its answers, at this revision.
    Seen { root: PathBuf, agent: Agent, stop: usize, exchange: Option<usize>, revision: u32 },
    /// An MCP tool call, answered with [`Reply::Tool`].
    Tool { id: u64, root: PathBuf, agent: Agent, model: Option<String>, name: String, arguments: Value },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Welcome,
    /// The daemon is from another build of tourer.
    WrongBuild,
    Tours { root: PathBuf, tours: Vec<TourSummary> },
    State { root: PathBuf, state: Box<State> },
    Tool { id: u64, text: String, is_error: bool },
    Error { message: String },
}

/// A tour, hosted or saved, as the start screen lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TourSummary {
    pub agent: Agent,
    pub title: String,
    pub stops: usize,
    pub questions: usize,
    pub planning: bool,
}

impl TourSummary {
    fn new(agent: Agent, tour: &Tour, planning: bool) -> Self {
        TourSummary {
            agent,
            title: tour.title.clone(),
            stops: tour.stops.len(),
            questions: tour.stops.iter().map(|s| s.thread.len()).sum(),
            planning,
        }
    }
}

/// Where a running daemon listens; written by the daemon, read by front-ends.
#[derive(Serialize, Deserialize)]
struct Address {
    port: u16,
    token: String,
    build: String,
}

/// tourer's own folder for the daemon's address, its log and saved tours.
pub fn data_dir() -> Result<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
    };
    let dir = base.context("cannot find a folder for tourer's data")?.join("tourer");
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    Ok(dir)
}

/// Identifies this build of tourer, so a front-end never talks to a stale daemon.
fn build_id() -> String {
    let modified = std::env::current_exe()
        .and_then(|exe| exe.metadata()?.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    format!("{}-{modified}", env!("CARGO_PKG_VERSION"))
}

fn write_line(out: &mut impl Write, msg: &impl Serialize) -> Result<()> {
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    out.write_all(&line)?;
    out.flush()?;
    Ok(())
}

// ---- Front-end side ----

/// A connection to the daemon. Replies are read from the stream returned by [`connect`].
pub struct Client {
    stream: TcpStream,
}

impl Client {
    pub fn send(&mut self, req: &Request) -> Result<()> {
        write_line(&mut self.stream, req).context("lost the connection to the tourer daemon")
    }
}

/// Connects to the daemon, starting it if it is not running or is from another build.
pub fn connect() -> Result<(Client, impl Iterator<Item = Reply>)> {
    let dir = data_dir()?;
    let build = build_id();
    if let Ok(found) = try_connect(&dir, &build) {
        return Ok(found);
    }
    start(&dir, &build)?;
    let started = Instant::now();
    loop {
        match try_connect(&dir, &build) {
            Ok(found) => return Ok(found),
            Err(err) if started.elapsed() > Duration::from_secs(10) => {
                let log = dir.join("daemon.log");
                return Err(err.context(format!("the tourer daemon did not start; see {}", log.display())));
            }
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn try_connect(dir: &Path, build: &str) -> Result<(Client, impl Iterator<Item = Reply> + use<>)> {
    let addr: Address = serde_json::from_slice(&std::fs::read(dir.join("daemon.json"))?)?;
    if addr.build != build {
        bail!("the daemon is from another build");
    }
    let stream = TcpStream::connect(("127.0.0.1", addr.port))?;
    let mut client = Client { stream: stream.try_clone()? };
    client.send(&Request::Hello { token: addr.token, build: build.to_string() })?;
    let mut replies = BufReader::new(stream)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Reply>(&line).ok());
    match replies.next() {
        Some(Reply::Welcome) => Ok((client, replies)),
        _ => bail!("the daemon refused the connection"),
    }
}

/// Starts a daemon in the background, running a copy of this executable so that the
/// original can be rebuilt while the daemon runs.
fn start(dir: &Path, build: &str) -> Result<()> {
    let exe = std::env::current_exe()?;
    let copy = dir.join(format!("daemon-{build}{}", std::env::consts::EXE_SUFFIX));
    if !copy.exists() {
        std::fs::copy(&exe, &copy).with_context(|| format!("cannot copy tourer to {}", copy.display()))?;
    }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("daemon.log"))?;
    let mut cmd = Command::new(&copy);
    cmd.args(["--daemon", "--build", build]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
    detach(&mut cmd)?;
    Ok(())
}

#[cfg(windows)]
fn detach(cmd: &mut Command) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // Leave the caller's job object so the daemon outlives it, where the job allows that.
    let flags = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
    if cmd.creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB).spawn().is_err() {
        cmd.creation_flags(flags).spawn().context("cannot start the tourer daemon")?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn detach(cmd: &mut Command) -> Result<()> {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0).spawn().context("cannot start the tourer daemon")?;
    Ok(())
}

// ---- Daemon side ----

/// One folder's tour, shared by the threads serving clients.
pub struct Shared {
    hosted: Mutex<Hosted>,
    changed: Condvar,
    /// Where the tour is saved.
    file: PathBuf,
}

struct Hosted {
    engine: Engine,
    /// Counts changes, so watchers know when to send an update.
    version: u64,
}

impl Shared {
    /// Changes the engine and tells everyone waiting on it.
    pub fn update<R>(&self, f: impl FnOnce(&mut Engine) -> R) -> R {
        let mut hosted = self.hosted.lock().unwrap();
        hosted.version += 1;
        let r = f(&mut hosted.engine);
        drop(hosted);
        self.changed.notify_all();
        r
    }

    /// Waits up to `timeout` while `busy` holds, then returns the engine, locked.
    pub fn wait(&self, timeout: Duration, busy: impl Fn(&Engine) -> bool) -> EngineGuard<'_> {
        let hosted = self.hosted.lock().unwrap();
        EngineGuard(self.changed.wait_timeout_while(hosted, timeout, |h| busy(&h.engine)).unwrap().0)
    }

    /// Saves the tour, once it has stops and planning is over.
    fn save(&self, tour: Option<Tour>) {
        let Some(tour) = tour else { return };
        if let Err(err) = save(&self.file, &tour) {
            eprintln!("cannot save {}: {err:#}", self.file.display());
        }
    }

    fn tour_to_save(engine: &Engine) -> Option<Tour> {
        (!engine.state.planning && !engine.state.tour.stops.is_empty()).then(|| engine.state.tour.clone())
    }
}

/// What one MCP client last read of each stop, by stop index, so outlines can point out updates.
pub type Reads = Mutex<HashMap<usize, u32>>;

pub struct EngineGuard<'a>(MutexGuard<'a, Hosted>);

impl std::ops::Deref for EngineGuard<'_> {
    type Target = Engine;
    fn deref(&self) -> &Engine {
        &self.0.engine
    }
}

struct Daemon {
    dir: PathBuf,
    token: String,
    build: String,
    port: u16,
    tours: Mutex<HashMap<(PathBuf, Agent), Arc<Shared>>>,
    clients: AtomicUsize,
    last_client: Mutex<Instant>,
}

pub fn run(build: String) -> Result<()> {
    let dir = data_dir()?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let random = RandomState::new();
    let token = format!("{:016x}{:016x}", random.hash_one(1), random.hash_one(2));
    let addr = Address { port, token: token.clone(), build: build.clone() };
    let tmp = dir.join(format!("daemon.{port}.json"));
    std::fs::write(&tmp, serde_json::to_vec(&addr)?)?;
    std::fs::rename(&tmp, dir.join("daemon.json"))?;
    remove_old_copies(&dir, &build);
    eprintln!("tourer daemon {build} listening on port {port}");

    let daemon = Arc::new(Daemon {
        dir,
        token,
        build,
        port,
        tours: Mutex::new(HashMap::new()),
        clients: AtomicUsize::new(0),
        last_client: Mutex::new(Instant::now()),
    });
    let idle = daemon.clone();
    thread::spawn(move || idle.exit_when_idle());
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let daemon = daemon.clone();
        thread::spawn(move || {
            if let Err(err) = daemon.serve(stream) {
                eprintln!("client: {err:#}");
            }
        });
    }
    Ok(())
}

/// Deletes executables left by daemons of earlier builds. Ones still running stay.
fn remove_old_copies(dir: &Path, build: &str) {
    let ours = format!("daemon-{build}{}", std::env::consts::EXE_SUFFIX);
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("daemon-") && name != ours {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

impl Daemon {
    fn serve(self: &Arc<Self>, stream: TcpStream) -> Result<()> {
        let out = Arc::new(Mutex::new(stream.try_clone()?));
        let mut lines = BufReader::new(stream).lines();
        let hello = lines.next().context("no hello")??;
        match serde_json::from_str(&hello)? {
            Request::Hello { token, .. } if token != self.token => bail!("wrong token"),
            Request::Hello { build, .. } if build != self.build => {
                return write_line(&mut *out.lock().unwrap(), &Reply::WrongBuild);
            }
            Request::Hello { .. } => write_line(&mut *out.lock().unwrap(), &Reply::Welcome)?,
            _ => bail!("expected hello"),
        }

        self.clients.fetch_add(1, Ordering::SeqCst);
        let connected = Arc::new(AtomicBool::new(true));
        let mut reads = HashMap::new();
        let result = (|| -> Result<()> {
            for line in lines {
                let request: Request = serde_json::from_str(&line?)?;
                if let Err(err) = self.handle(request, &out, &connected, &mut reads) {
                    write_line(&mut *out.lock().unwrap(), &Reply::Error { message: format!("{err:#}") })?;
                }
            }
            Ok(())
        })();
        connected.store(false, Ordering::SeqCst);
        self.clients.fetch_sub(1, Ordering::SeqCst);
        *self.last_client.lock().unwrap() = Instant::now();
        result
    }

    fn handle(
        &self,
        request: Request,
        out: &Arc<Mutex<TcpStream>>,
        connected: &Arc<AtomicBool>,
        reads: &mut HashMap<(PathBuf, Agent), Arc<Reads>>,
    ) -> Result<()> {
        match request {
            Request::Hello { .. } => bail!("hello sent twice"),
            Request::List { root } => {
                let tours = self.list(&root);
                write_line(&mut *out.lock().unwrap(), &Reply::Tours { root, tours })?;
            }
            Request::Open { root, agent, model, replan } => {
                let shared = self.tour(&root, agent, model)?;
                if replan {
                    shared.update(|e| e.replan())?;
                }
                let (out, connected) = (out.clone(), connected.clone());
                thread::spawn(move || watch(&shared, &root, &out, &connected));
            }
            Request::Ask { root, agent, stop, question, code } => {
                let shared = self.hosted(root, agent)?;
                shared.update(|e| e.ask(stop, &question, code)).context("no such stop")?;
            }
            Request::Seen { root, agent, stop, exchange, revision } => {
                let shared = self.hosted(root, agent)?;
                let tour = shared.update(|e| {
                    e.mark_seen(stop, exchange, revision);
                    Shared::tour_to_save(e)
                });
                shared.save(tour);
            }
            Request::Tool { id, root, agent, model, name, arguments } => {
                let read = reads.entry((root.clone(), agent)).or_default().clone();
                let shared = self.tour(&root, agent, model)?;
                let out = out.clone();
                thread::spawn(move || {
                    let (text, is_error) = match mcp::call(&shared, &name, &arguments, &read) {
                        Ok(text) => (text, false),
                        Err(err) => (format!("{err:#}"), true),
                    };
                    let _ = write_line(&mut *out.lock().unwrap(), &Reply::Tool { id, text, is_error });
                });
            }
        }
        Ok(())
    }

    /// A tour that a front-end has opened.
    fn hosted(&self, root: PathBuf, agent: Agent) -> Result<Arc<Shared>> {
        self.tours.lock().unwrap().get(&(root, agent)).cloned().context("open the tour first")
    }

    /// A folder's tours, one per agent at most: hosted ones, else saved ones.
    fn list(&self, root: &Path) -> Vec<TourSummary> {
        let tours = self.tours.lock().unwrap();
        Agent::ALL
            .into_iter()
            .filter_map(|agent| match tours.get(&(root.to_path_buf(), agent)) {
                Some(shared) => {
                    let state = &shared.hosted.lock().unwrap().engine.state;
                    Some(TourSummary::new(agent, &state.tour, state.planning))
                }
                None => self.saved(root, agent).map(|tour| TourSummary::new(agent, &tour, false)),
            })
            .collect()
    }

    fn saved(&self, root: &Path, agent: Agent) -> Option<Tour> {
        std::fs::read(self.tour_file(root, agent))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Tour>(&bytes).ok())
            .filter(|t| Path::new(&t.root) == root && !t.stops.is_empty())
    }

    /// A folder's tour by an agent: hosted already, saved earlier, or newly planned.
    fn tour(&self, root: &Path, agent: Agent, model: Option<String>) -> Result<Arc<Shared>> {
        let mut tours = self.tours.lock().unwrap();
        let key = (root.to_path_buf(), agent);
        if let Some(shared) = tours.get(&key) {
            // A tour that failed before it had any stops is planned again.
            let state = &shared.hosted.lock().unwrap().engine.state;
            if state.planning || !state.tour.stops.is_empty() || state.error.is_none() {
                return Ok(shared.clone());
            }
        }
        let file = self.tour_file(root, agent);
        let (tx, rx) = mpsc::channel();
        let engine = Engine::new(root.to_path_buf(), agent, model, tx, self.saved(root, agent))?;
        let hosted = Mutex::new(Hosted { engine, version: 0 });
        let shared = Arc::new(Shared { hosted, changed: Condvar::new(), file });
        let pump = shared.clone();
        thread::spawn(move || {
            for (tag, event) in rx {
                let turn_ended = matches!(event, crate::agent::Event::TurnDone(_) | crate::agent::Event::Exited(_));
                let tour = pump.update(|e| {
                    e.on_agent(tag, event);
                    if turn_ended { Shared::tour_to_save(e) } else { None }
                });
                pump.save(tour);
            }
        });
        tours.insert(key, shared.clone());
        Ok(shared)
    }

    /// The file a folder's tour is saved in, named after the folder's path and the agent.
    fn tour_file(&self, root: &Path, agent: Agent) -> PathBuf {
        let name: String = root
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
            .collect();
        self.dir.join("tours").join(format!("{}.{agent}.json", name.trim_matches('_')))
    }

    fn exit_when_idle(&self) {
        loop {
            thread::sleep(Duration::from_secs(15));
            let idle = self.clients.load(Ordering::SeqCst) == 0
                && self.last_client.lock().unwrap().elapsed() > IDLE_EXIT
                && !self.tours.lock().unwrap().values().any(|s| s.hosted.lock().unwrap().engine.is_busy());
            if !idle {
                continue;
            }
            for shared in self.tours.lock().unwrap().values() {
                shared.hosted.lock().unwrap().engine.stop_agents();
            }
            // A newer daemon may have taken over the address file.
            let path = self.dir.join("daemon.json");
            let ours = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Address>(&b).ok())
                .is_some_and(|a| a.port == self.port);
            if ours {
                let _ = std::fs::remove_file(path);
            }
            eprintln!("tourer daemon idle, exiting");
            std::process::exit(0);
        }
    }
}

fn save(file: &Path, tour: &Tour) -> Result<()> {
    std::fs::create_dir_all(file.parent().ok_or_else(|| anyhow!("bad tour file"))?)?;
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(tour)?)?;
    std::fs::rename(&tmp, file)?;
    Ok(())
}

/// Sends a client the tour's state now and after every change, until it disconnects.
fn watch(shared: &Shared, root: &Path, out: &Mutex<TcpStream>, connected: &AtomicBool) {
    let mut seen = None;
    while connected.load(Ordering::SeqCst) {
        let hosted = shared.hosted.lock().unwrap();
        let (hosted, _) = shared
            .changed
            .wait_timeout_while(hosted, Duration::from_secs(1), |h| seen == Some(h.version))
            .unwrap();
        if seen == Some(hosted.version) {
            continue;
        }
        seen = Some(hosted.version);
        let reply = Reply::State { root: root.to_path_buf(), state: Box::new(hosted.engine.state.clone()) };
        drop(hosted);
        if write_line(&mut *out.lock().unwrap(), &reply).is_err() {
            return;
        }
        thread::sleep(UPDATE_GAP);
    }
}
