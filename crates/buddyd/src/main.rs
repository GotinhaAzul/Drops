mod geometry;
mod recovery;
mod rules;
use anyhow::{anyhow, bail, Context, Result};
use buddy_proto::{Blob, Command, Snapshot, MAX_BLOBS, MAX_FRAME};
use clap::Parser;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{broadcast, mpsc, Mutex},
};
use tokio_util::sync::CancellationToken;
#[derive(Parser)]
#[command(about = "Drops desktop companion daemon")]
struct Args {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    headless: bool,
}
#[derive(Clone, Deserialize)]
struct Config {
    #[serde(default)]
    appearance: Value,
    provider: HashMap<String, buddy_ai::ProviderConfig>,
    #[serde(default = "local")]
    active_provider: String,
    #[serde(default)]
    project_dir: Option<PathBuf>,
    #[serde(default)]
    instructions: String,
    #[serde(default)]
    external_tools: Vec<buddy_agent::ExternalTool>,
    #[serde(default)]
    anchor_rules: Vec<rules::AnchorRule>,
}
fn local() -> String {
    "local".into()
}
struct Running {
    abort: Option<tokio::task::AbortHandle>,
    cancel: CancellationToken,
    approve: mpsc::UnboundedSender<bool>,
}
struct App {
    persist_gate: Arc<Mutex<()>>,
    dirty: bool,
    snapshot: Snapshot,
    running: HashMap<String, Running>,
    config: Config,
    data: PathBuf,
    headless: bool,
    tx: broadcast::Sender<Value>,
}
type Shared = Arc<Mutex<App>>;
impl App {
    fn publish(&self) {
        let _ = self.tx.send(json!({"type":"state","state":self.snapshot}));
    }
    fn index(&self, id: &str) -> Result<usize> {
        self.snapshot
            .blobs
            .iter()
            .position(|b| b.id == id)
            .ok_or_else(|| anyhow!("unknown blob"))
    }
}
fn default_dir(env: &str, suffix: &str) -> PathBuf {
    std::env::var_os(env)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(
                if env == "XDG_CONFIG_HOME" {
                    ".config"
                } else {
                    ".local/share"
                },
            )
        })
        .join(suffix)
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    buddy_proto::validate_socket_environment()?;
    let path = args
        .config
        .unwrap_or_else(|| default_dir("XDG_CONFIG_HOME", "drops/config.toml"));
    let config: Config = toml::from_str(&std::fs::read_to_string(&path).with_context(|| {
        format!(
            "Read {}; copy examples/config.toml and configure a provider",
            path.display()
        )
    })?)?;
    if let Some(root) = &config.project_dir {
        let resolved = std::fs::canonicalize(root)
            .with_context(|| format!("project_dir must exist: {}", root.display()))?;
        if resolved == std::path::Path::new("/") {
            bail!("project_dir cannot be filesystem root");
        }
    }
    if !config.provider.contains_key(&config.active_provider) {
        bail!("active_provider does not exist")
    }
    let data = args
        .data_dir
        .unwrap_or_else(|| default_dir("XDG_DATA_HOME", "drops"));
    std::fs::create_dir_all(&data)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700))?;
    let mut snapshot: Snapshot = if data.join("blobs.json").exists() {
        serde_json::from_slice(&std::fs::read(data.join("blobs.json"))?)?
    } else {
        Snapshot {
            appearance: config.appearance.clone(),
            panel_open: false,
            blobs: vec![],
            version: 1,
        }
    };
    snapshot.appearance = config.appearance.clone();
    if snapshot.blobs.len() > MAX_BLOBS {
        bail!("saved state exceeds the three-blob limit");
    }
    let mut ids = std::collections::HashSet::new();
    for b in &mut snapshot.blobs {
        buddy_agent::session_path(&data.join("sessions"), &b.id)?;
        if !ids.insert(b.id.clone()) {
            bail!("duplicate blob id in saved state");
        }
        recovery::hydrate(b, &data.join("sessions"))?;
        b.state = "idle".into();
        b.approval = None;
    }
    let socket = buddy_proto::socket_path();
    let parent = socket.parent().context("socket parent")?;
    std::fs::create_dir_all(parent)?;
    if socket.exists() {
        if UnixStream::connect(&socket).await.is_ok() {
            bail!("daemon already listening")
        };
        let meta = std::fs::symlink_metadata(&socket)?;
        use std::os::unix::fs::FileTypeExt;
        if !meta.file_type().is_socket() {
            bail!("refusing to replace non-socket path")
        };
        std::fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let (tx, _) = broadcast::channel(512);
    let app = Arc::new(Mutex::new(App {
        persist_gate: Arc::new(Mutex::new(())),
        dirty: false,
        snapshot,
        running: HashMap::new(),
        config,
        data,
        headless: args.headless,
        tx,
    }));
    let stop = CancellationToken::new();
    if !args.headless {
        refresh_anchors(&app).await;
        tokio::spawn(geometry::watch(app.clone(), stop.clone()));
        let (etx, mut erx) = mpsc::unbounded_channel();
        let token = stop.clone();
        tokio::spawn(async move {
            if let Err(e) = buddy_anchor::subscribe(etx, token).await {
                eprintln!("Hyprland events unavailable: {e:#}");
            }
        });
        let a = app.clone();
        tokio::spawn(async move {
            while erx.recv().await.is_some() {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                while erx.try_recv().is_ok() {}
                refresh_anchors(&a).await;
            }
        });
    }
    let saved = app.clone();
    let persistence_stop = stop.clone();
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            tokio::select! {_=persistence_stop.cancelled()=>break,_=ticks.tick()=>{if saved.lock().await.dirty{if let Err(e)=persist(&saved).await{eprintln!("Metadata save retry: {e:#}");}}}}
        }
    });
    eprintln!("Drops listening on {}", socket.display());
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {s=listener.accept()=>{let(s,_)=s?;let a=app.clone();tokio::spawn(async move{if let Err(e)=client(s,a).await{eprintln!("client: {e:#}");}});},_=tokio::signal::ctrl_c()=>break, _=terminate.recv()=>break}
    }
    stop.cancel();
    {
        let a = app.lock().await;
        for r in a.running.values() {
            r.cancel.cancel();
        }
    }
    let grace = async {
        loop {
            if app.lock().await.running.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(2), grace)
        .await
        .is_err()
    {
        let a = app.lock().await;
        for r in a.running.values() {
            if let Some(abort) = &r.abort {
                abort.abort();
            }
        }
        drop(a);
        tokio::task::yield_now().await;
    }
    persist(&app).await?;
    std::fs::remove_file(socket)?;
    Ok(())
}
/// Keep anchor identity coordinate-free and persist only a decoration offset.
fn place_drop(mut reference: Value, x: i32, y: i32) -> (Value, [i32; 2]) {
    let origin = [
        reference["x"].as_i64().unwrap_or(0) as i32,
        reference["y"].as_i64().unwrap_or(0) as i32,
    ];
    let offset = [x.saturating_sub(origin[0]), y.saturating_sub(origin[1])];
    reference["x"] = json!(x);
    reference["y"] = json!(y);
    (reference, offset)
}

async fn refresh_anchors(app: &Shared) -> bool {
    let blobs: Vec<_> = app
        .lock()
        .await
        .snapshot
        .blobs
        .iter()
        .map(|b| (b.id.clone(), b.placement.clone(), b.offset))
        .collect();
    if blobs.is_empty() {
        return true;
    }
    let Ok(desktop) = buddy_anchor::snapshot().await else {
        return false;
    };
    let mut changed = false;
    for (id, placement, original_offset) in blobs {
        let mut offset = original_offset;
        if let Ok(anchor) =
            serde_json::from_value::<buddy_anchor::Anchor>(placement["anchor"].clone())
        {
            if let Ok(mut p) = buddy_anchor::refresh_from_snapshot(&desktop, &anchor) {
                if p.anchor != anchor {
                    offset = [
                        (placement["x"].as_i64().unwrap_or(i64::from(p.x)) as i32)
                            .saturating_sub(p.x),
                        (placement["y"].as_i64().unwrap_or(i64::from(p.y)) as i32)
                            .saturating_sub(p.y),
                    ];
                }
                p.x = p.x.saturating_add(offset[0]);
                p.y = p.y.saturating_add(offset[1]);
                let mut a = app.lock().await;
                if let Ok(i) = a.index(&id) {
                    if a.snapshot.blobs[i].placement["anchor"] != placement["anchor"]
                        || a.snapshot.blobs[i].offset != original_offset
                    {
                        continue;
                    }
                    let placement = serde_json::to_value(p).unwrap();
                    if a.snapshot.blobs[i].placement != placement {
                        a.snapshot.blobs[i].placement = placement;
                        a.snapshot.blobs[i].offset = offset;
                        changed = true;
                    }
                }
            }
        }
    }
    if changed {
        let mut a = app.lock().await;
        a.dirty = true;
        a.publish();
    }
    true
}
async fn client(stream: UnixStream, app: Shared) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let mut subscription = None;
    let mut bytes = Vec::new();
    loop {
        let value = if let Some(ref mut rx) = subscription {
            tokio::select! {r=read_frame(&mut read,&mut bytes)=>{if !r?{break};Some(serde_json::from_slice::<Command>(&bytes))},event=broadcast::Receiver::recv(rx)=>{match event{Ok(e)=>{write.write_all(format!("{e}\n").as_bytes()).await?;},Err(broadcast::error::RecvError::Lagged(_))=>{let a=app.lock().await;write.write_all(format!("{}\n",json!({"type":"state","state":a.snapshot})).as_bytes()).await?},Err(_)=>break};None}}
        } else {
            if !read_frame(&mut read, &mut bytes).await? {
                break;
            };
            Some(serde_json::from_slice::<Command>(&bytes))
        };
        if let Some(cmd) = value {
            bytes.clear();
            let reply = match cmd {
                Ok(Command::Watch) => {
                    let a = app.lock().await;
                    subscription = Some(a.tx.subscribe());
                    json!({"type":"state","state":a.snapshot})
                }
                Ok(cmd) => match handle(cmd, &app).await {
                    Ok(v) => v,
                    Err(e) => json!({"type":"error","message":e.to_string()}),
                },
                Err(e) => json!({"type":"error","message":e.to_string()}),
            };
            write.write_all(format!("{reply}\n").as_bytes()).await?;
        }
    }
    Ok(())
}
async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(
    r: &mut R,
    out: &mut Vec<u8>,
) -> Result<bool> {
    loop {
        let b = r.fill_buf().await?;
        if b.is_empty() {
            return Ok(!out.is_empty());
        };
        let n = b
            .iter()
            .position(|c| *c == b'\n')
            .map_or(b.len(), |i| i + 1);
        if out.len() + n > MAX_FRAME {
            bail!("frame too large")
        };
        let done = b[n - 1] == b'\n';
        out.extend_from_slice(&b[..n]);
        r.consume(n);
        if done {
            return Ok(true);
        }
    }
}
async fn handle(cmd: Command, app: &Shared) -> Result<Value> {
    if let Command::Send { id, text } = cmd {
        return send(app, id, text).await;
    }
    if let Command::Preview { x, y, use_cursor } = cmd {
        let (x, y) = if use_cursor && !app.lock().await.headless {
            buddy_anchor::cursor_position().await?
        } else {
            (x, y)
        };
        if app.lock().await.headless {
            return Ok(
                json!({"type":"preview","placement":{"x":x,"y":y,"label":"Headless workspace"},"geometry":{"x":x,"y":y,"width":160,"height":56}}),
            );
        }
        // Preview uses one desktop snapshot; no process metadata is captured.
        return match buddy_anchor::preview(x, y).await {
            Ok(mut value) => {
                value["type"] = json!("preview");
                Ok(value)
            }
            Err(_) => Ok(json!({"type":"preview","placement":null,"geometry":null})),
        };
    }
    if let Command::Drop {
        x,
        y,
        id,
        use_cursor,
    } = cmd
    {
        let (x, y) = if use_cursor && !app.lock().await.headless {
            buddy_anchor::cursor_position().await?
        } else {
            (x, y)
        };
        let headless = app.lock().await.headless;
        let reference = if headless {
            json!({"anchor":{"kind":"workspace","id":1},"x":x,"y":y,"visible":true,"label":"Headless workspace"})
        } else {
            serde_json::to_value(buddy_anchor::hit_test(x, y).await?)?
        };
        let (placement, offset) = place_drop(reference, x, y);
        let mut a = app.lock().await;
        let id = if let Some(id) = id {
            let i = a.index(&id)?;
            if a.running.contains_key(&id) {
                bail!("cancel the turn before moving its anchor")
            };
            a.snapshot.blobs[i].placement = placement;
            a.snapshot.blobs[i].offset = offset;
            id
        } else {
            if a.snapshot.blobs.len() >= MAX_BLOBS {
                bail!("maximum of three blobs")
            };
            let id = uuid::Uuid::new_v4().to_string();
            a.snapshot.blobs.push(Blob {
                offset,
                id: id.clone(),
                placement,
                state: "idle".into(),
                history: vec![],
                context: Value::Null,
                approval: None,
            });
            id
        };
        a.snapshot.panel_open = false;
        a.publish();
        drop(a);
        persist(app).await?;
        return Ok(json!({"type":"ok","id":id}));
    }
    let mut a = app.lock().await;
    match cmd {
        Command::Status => return Ok(json!({"type":"state","state":a.snapshot})),
        Command::Toggle => a.snapshot.panel_open = !a.snapshot.panel_open,
        Command::Confirm { id, approve } => {
            let i = a.index(&id)?;
            if a.snapshot.blobs[i].approval.is_none() {
                bail!("no pending approval")
            };
            a.running
                .get(&id)
                .filter(|r| !r.cancel.is_cancelled())
                .context("agent not running or stopping")?
                .approve
                .send(approve)?;
            a.snapshot.blobs[i].approval = None;
            a.snapshot.blobs[i].state = "thinking".into();
        }
        Command::Cancel { id } => {
            if let Some(id) = &id {
                a.index(id)?;
            }
            let ids: Vec<String> = id
                .map(|id| vec![id])
                .unwrap_or_else(|| a.running.keys().cloned().collect());
            for id in ids {
                let active = a.running.contains_key(&id);
                if let Some(r) = a.running.get(&id) {
                    r.cancel.cancel();
                    if let Some(abort) = r.abort.clone() {
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            abort.abort();
                        });
                    }
                }
                if active {
                    if let Ok(i) = a.index(&id) {
                        a.snapshot.blobs[i].approval = None;
                        a.snapshot.blobs[i].state = "stopping".into();
                    }
                }
            }
        }
        Command::Recall { id } => {
            a.index(&id)?;
            a.snapshot.panel_open = true;
        }
        Command::Remove { id } => {
            let i = a.index(&id)?;
            if a.running.contains_key(&id) {
                bail!("cancel and wait for idle before removing")
            };
            let session = buddy_agent::session_path(&a.data.join("sessions"), &id)?;
            if session.exists() {
                std::fs::remove_file(session)?;
            }
            a.snapshot.blobs.remove(i);
        }
        Command::Context { id } => {
            let i = a.index(&id)?;
            return Ok(json!({"type":"context","context":a.snapshot.blobs[i].context}));
        }
        Command::Export { id } => {
            a.index(&id)?;
            let p = buddy_agent::session_path(&a.data.join("sessions"), &id)?;
            let content = if p.exists() {
                std::fs::read_to_string(p)?
            } else {
                String::new()
            };
            return Ok(json!({"type":"export","jsonl":content}));
        }
        Command::Clear { id } => {
            let i = a.index(&id)?;
            if a.running.contains_key(&id) {
                bail!("session is busy")
            };
            let p = buddy_agent::session_path(&a.data.join("sessions"), &id)?;
            if p.exists() {
                std::fs::remove_file(p)?
            };
            a.snapshot.blobs[i].history.clear();
            a.snapshot.blobs[i].context = Value::Null;
        }
        Command::Fork { id } => {
            let i = a.index(&id)?;
            if a.running.contains_key(&id) {
                bail!("session is busy")
            };
            if a.snapshot.blobs.len() >= MAX_BLOBS {
                bail!("maximum of three blobs")
            };
            let mut b = a.snapshot.blobs[i].clone();
            b.id = uuid::Uuid::new_v4().to_string();
            let src = buddy_agent::session_path(&a.data.join("sessions"), &id)?;
            let dst = buddy_agent::session_path(&a.data.join("sessions"), &b.id)?;
            if src.exists() {
                std::fs::copy(src, dst)?;
            };
            let id = b.id.clone();
            a.snapshot.blobs.push(b);
            a.publish();
            drop(a);
            persist(app).await?;
            return Ok(json!({"type":"ok","id":id}));
        }
        _ => bail!("unsupported command"),
    }
    a.publish();
    drop(a);
    persist(app).await?;
    Ok(json!({"type":"ok"}))
}
async fn send(app: &Shared, id: String, text: String) -> Result<Value> {
    if text.trim().is_empty() {
        bail!("empty message");
    }
    let mut a = app.lock().await;
    let i = a.index(&id)?;
    if a.running.contains_key(&id) {
        bail!("blob already running");
    }
    let placement = a.snapshot.blobs[i].placement.clone();
    let headless = a.headless;
    let configuration = a.config.clone();
    let session_dir = a.data.join("sessions");
    let project_dir = configuration
        .project_dir
        .clone()
        .unwrap_or_else(|| a.data.join("projects").join(&id));
    let cancel = CancellationToken::new();
    let (atx, arx) = mpsc::unbounded_channel();
    let (etx, mut erx) = mpsc::unbounded_channel::<Value>();
    // Reserve the session before context capture. Controls on this same bridge
    // remain responsive, and clear/fork/drop cannot race an accepted send.
    a.running.insert(
        id.clone(),
        Running {
            abort: None,
            cancel: cancel.clone(),
            approve: atx,
        },
    );
    let b = &mut a.snapshot.blobs[i];
    b.state = "thinking".into();
    b.approval = None;
    b.history.push(json!({"type":"user","text":text}));
    a.publish();
    drop(a);
    let blob_id = id.clone();
    let task = tokio::spawn(async move {
        let result: Result<()> = async {
            let context = if headless {
                json!({"mode":"headless","cwd":configuration.project_dir})
            } else {
                let anchor = serde_json::from_value(placement["anchor"].clone())?;
                tokio::select! {
                    _=cancel.cancelled()=>return Ok(()),
                    result=buddy_anchor::context(&anchor)=>result?,
                }
            };
            if cancel.is_cancelled() {
                return Ok(());
            }
            let _ = etx.send(json!({"type":"context","context":context}));
            let class = context["window"]["class"]
                .as_str()
                .unwrap_or("")
                .to_lowercase();
            let mut allow_shell = matches!(
                class.as_str(),
                "alacritty"
                    | "kitty"
                    | "foot"
                    | "footclient"
                    | "com.mitchellh.ghostty"
                    | "ghostty"
                    | "org.wezfurlong.wezterm"
                    | "wezterm"
            );
            let mut instructions = configuration.instructions.clone();
            for rule in configuration
                .anchor_rules
                .iter()
                .filter(|r| r.matches(&context))
            {
                instructions.push('\n');
                instructions.push_str(&rule.instructions);
                allow_shell |= !headless && rule.allow_shell;
            }
            let config = buddy_agent::AgentConfig {
                provider: configuration.provider[&configuration.active_provider].clone(),
                session_dir,
                project_dir,
                instructions,
                max_turns: 16,
                external_tools: configuration.external_tools,
            };
            buddy_agent::run(
                config,
                blob_id,
                text,
                context,
                allow_shell,
                etx.clone(),
                arx,
                cancel,
            )
            .await
        }
        .await;
        if let Err(e) = result {
            let _ = etx.send(json!({"type":"error","text":e.to_string()}));
        }
    });
    if let Some(r) = app.lock().await.running.get_mut(&id) {
        r.abort = Some(task.abort_handle());
    }
    let shared = app.clone();
    tokio::spawn(async move {
        let mut last_publish = tokio::time::Instant::now() - std::time::Duration::from_secs(1);
        while let Some(event) = erx.recv().await {
            let mut a = shared.lock().await;
            let cancelled = a.running.get(&id).is_none_or(|r| r.cancel.is_cancelled());
            if event["type"] == "approval" && cancelled {
                continue;
            }
            if let Ok(i) = a.index(&id) {
                let b = &mut a.snapshot.blobs[i];
                if event["type"] == "context" {
                    b.context = event["context"].clone();
                    a.publish();
                    continue;
                }
                if event["type"] == "error" && event["text"] != "cancelled" {
                    b.state = "error".into();
                }
                if event["type"] == "approval" {
                    b.state = "waiting".into();
                    b.approval = Some(event.clone());
                }
                if event["type"] == "text" || event["type"] == "thinking" {
                    if let Some(last) = b
                        .history
                        .last_mut()
                        .filter(|last| last["type"] == event["type"])
                    {
                        last["text"] = json!(format!(
                            "{}{}",
                            last["text"].as_str().unwrap_or(""),
                            event["text"].as_str().unwrap_or("")
                        ));
                    } else {
                        b.history.push(event.clone());
                    }
                } else {
                    b.history.push(event.clone());
                }
                if b.history.len() > 300 {
                    b.history.drain(..100);
                }
                let _ =
                    a.tx.send(json!({"type":"agent_event","id":id,"event":event}));
                let delta = event["type"] == "text" || event["type"] == "thinking";
                if !delta || last_publish.elapsed() >= std::time::Duration::from_millis(40) {
                    a.publish();
                    last_publish = tokio::time::Instant::now();
                }
            }
        }
        let mut a = shared.lock().await;
        a.running.remove(&id);
        if let Ok(i) = a.index(&id) {
            if a.snapshot.blobs[i].state != "error" {
                a.snapshot.blobs[i].state = "idle".into();
            }
            a.snapshot.blobs[i].approval = None;
        }
        a.publish();
        drop(a);
        if let Err(e) = persist(&shared).await {
            eprintln!("Session metadata save failed: {e:#}");
        }
    });
    // The worker and drain task exist before any fallible disk operation.
    persist(app).await?;
    Ok(json!({"type":"ok"}))
}

/// Serialize disk writes and copy state briefly under the lock. Encoding and
/// filesystem I/O run on the blocking pool, outside the global state mutex.
async fn persist(app: &Shared) -> Result<()> {
    let gate = app.lock().await.persist_gate.clone();
    let _guard = gate.lock().await;
    let (data, state) = {
        let mut a = app.lock().await;
        a.dirty = false;
        (a.data.clone(), a.snapshot.clone())
    };
    let result = tokio::task::spawn_blocking(move || -> Result<()> {
        let path = data.join("blobs.json");
        std::fs::write(path.with_extension("tmp"), serde_json::to_vec(&state)?)?;
        std::fs::rename(path.with_extension("tmp"), path)?;
        Ok(())
    })
    .await?;
    if result.is_err() {
        app.lock().await.dirty = true;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drop_keeps_cursor_position_as_a_persisted_offset() {
        let reference = json!({"anchor":{"kind":"workspace","id":1},"x":1366,"y":0,"visible":true});
        let (placed, offset) = place_drop(reference, 2497, 219);
        assert_eq!(offset, [1131, 219]);
        assert_eq!(placed["x"], 2497);
        assert_eq!(placed["y"], 219);
        let restored:Blob=serde_json::from_value(json!({"id":"test","placement":placed,"offset":offset,"state":"idle","history":[],"context":null,"approval":null})).unwrap();
        assert_eq!(restored.offset, [1131, 219]);
    }
    #[tokio::test]
    async fn partial_frame_survives_interleaved_events() {
        let (input, mut sender) = tokio::io::duplex(128);
        let mut reader = BufReader::new(input);
        let mut frame = Vec::new();
        sender.write_all(b"{\"type\":").await.unwrap();
        // A watch event wins while this command is incomplete. The caller must
        // retain consumed bytes before starting the next read future.
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(10),
            read_frame(&mut reader, &mut frame)
        )
        .await
        .is_err());
        sender.write_all(b"\"status\"}\n").await.unwrap();
        assert!(read_frame(&mut reader, &mut frame).await.unwrap());
        assert!(matches!(
            serde_json::from_slice::<Command>(&frame).unwrap(),
            Command::Status
        ));
    }
    #[tokio::test]
    async fn oversized_frames_are_rejected() {
        let data = vec![b'x'; MAX_FRAME + 1];
        let mut reader = BufReader::new(data.as_slice());
        assert!(read_frame(&mut reader, &mut Vec::new())
            .await
            .unwrap_err()
            .to_string()
            .contains("too large"));
    }
}
