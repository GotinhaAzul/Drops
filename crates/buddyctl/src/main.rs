use anyhow::{bail, Context, Result};
use buddy_proto::{socket_path, Command};
use clap::{Parser, Subcommand};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};
#[derive(Parser)]
#[command(name = "buddyctl", about = "Drops desktop companion control")]
struct Args {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    Status {
        #[arg(long)]
        waybar: bool,
    },
    Watch,
    Toggle,
    Configure {
        base_url: String,
        model: String,
        #[arg(long, default_value = "")]
        api_key_env: String,
        #[arg(long)]
        vision: Option<bool>,
        #[arg(long)]
        capture_content: Option<bool>,
        #[arg(long)]
        screenshots: Option<bool>,
    },
    Create {
        #[arg(long, default_value="drop",value_parser=["drop","cat","sprout"])]
        appearance: String,
    },
    Style {
        id: String,
        #[arg(value_parser=["drop","cat","sprout"])]
        appearance: String,
    },
    Drop {
        x: i32,
        y: i32,
        #[arg(long)]
        id: Option<String>,
    },
    Send {
        id: String,
        text: String,
    },
    Confirm {
        id: String,
        #[arg(long)]
        deny: bool,
    },
    Cancel {
        id: Option<String>,
    },
    Recall {
        id: String,
    },
    Remove {
        id: String,
    },
    Context {
        id: String,
    },
    Export {
        id: String,
    },
    Clear {
        id: String,
    },
    Fork {
        id: String,
    },
    /// Bidirectional NDJSON bridge for Quickshell or scripts.
    Bridge,
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    buddy_proto::validate_socket_environment()?;
    let waybar = matches!(args.command, Action::Status { waybar: true });
    let stream = match UnixStream::connect(socket_path()).await {
        Ok(s) => s,
        Err(e) if waybar => {
            println!(
                "{}",
                serde_json::json!({"text":"●","class":"offline","tooltip":format!("Drops offline: {e}")})
            );
            return Ok(());
        }
        Err(e) => return Err(e).context("Drops daemon unavailable; start buddyd"),
    };
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    if matches!(args.command, Action::Bridge) {
        write.write_all(b"{\"type\":\"watch\"}\n").await?;
        let mut stdin = BufReader::new(tokio::io::stdin()).lines();
        loop {
            tokio::select! {line=lines.next_line()=>match line? {Some(l)=>println!("{l}"),None=>break},line=stdin.next_line()=>match line? {Some(l)=>{if l.len()>buddy_proto::MAX_FRAME {bail!("frame too large")};serde_json::from_str::<Command>(&l)?;write.write_all(format!("{l}\n").as_bytes()).await?},None=>break}}
        }
        return Ok(());
    }
    let watch = matches!(args.command, Action::Watch);
    let cmd = match args.command {
        Action::Status { .. } => Command::Status,
        Action::Watch => Command::Watch,
        Action::Toggle => Command::Toggle,
        Action::Configure {
            base_url,
            model,
            api_key_env,
            vision,
            capture_content,
            screenshots,
        } => Command::Configure {
            base_url,
            model,
            api_key_env,
            vision,
            capture_content,
            screenshots,
        },
        Action::Create { appearance } => Command::Create {
            appearance: serde_json::from_value(serde_json::json!(appearance))?,
        },
        Action::Style { id, appearance } => Command::Style {
            id,
            appearance: serde_json::from_value(serde_json::json!(appearance))?,
        },
        Action::Drop { x, y, id } => Command::Drop {
            x,
            y,
            id,
            use_cursor: false,
        },
        Action::Send { id, text } => Command::Send { id, text },
        Action::Confirm { id, deny } => Command::Confirm { id, approve: !deny },
        Action::Cancel { id } => Command::Cancel { id },
        Action::Recall { id } => Command::Recall { id },
        Action::Remove { id } => Command::Remove { id },
        Action::Context { id } => Command::Context { id },
        Action::Export { id } => Command::Export { id },
        Action::Clear { id } => Command::Clear { id },
        Action::Fork { id } => Command::Fork { id },
        Action::Bridge => unreachable!(),
    };
    write
        .write_all(format!("{}\n", serde_json::to_string(&cmd)?).as_bytes())
        .await?;
    while let Some(line) = lines.next_line().await? {
        if waybar {
            let v: serde_json::Value = serde_json::from_str(&line)?;
            let blobs = v["state"]["blobs"].as_array();
            let state = blobs
                .and_then(|b| {
                    b.iter()
                        .find(|x| x["state"] == "waiting")
                        .or_else(|| b.iter().find(|x| x["state"] == "capturing"))
                        .or_else(|| b.iter().find(|x| x["state"] == "thinking"))
                        .or_else(|| b.iter().find(|x| x["state"] == "error"))
                })
                .and_then(|b| b["state"].as_str())
                .unwrap_or("idle");
            println!(
                "{}",
                serde_json::json!({"text":"●","class":state,"tooltip":format!("Drops · {} companions · {state}",blobs.map_or(0,|b|b.len()))})
            );
        } else {
            println!("{line}");
        }
        if !watch {
            let v: serde_json::Value = serde_json::from_str(&line)?;
            if v["type"] == "error" {
                bail!("{}", v["message"])
            }
            break;
        }
    }
    Ok(())
}
