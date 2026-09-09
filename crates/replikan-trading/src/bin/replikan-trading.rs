//! JSON-lines command surface for local agents. This is not an MCP server.
use replikan_trading::{Config, Error, Intent, Result, Runtime, paper::PaperVenue};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{BufRead, Write};

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Capabilities,
    Prepare { intent: Box<Intent> },
    Abandon { client_order_id: String, reason: String },
    Dispatch { client_order_id: String },
    Reconcile { client_order_id: String },
    Cancel { client_order_id: String },
    Snapshot,
    Export,
}

fn now_ms() -> Result<i64> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error("system clock precedes epoch".into()))?;
    i64::try_from(elapsed.as_millis()).map_err(|_| Error("timestamp overflow".into()))
}

fn execute(runtime: &mut Runtime, venue: &mut PaperVenue, command: Command) -> Result<Value> {
    let now = now_ms()?;
    match command {
        Command::Capabilities => Ok(
            json!({"schema_version":1,"mode":"paper","protocol":"json-lines",
            "operations":["capabilities","prepare","abandon","dispatch","reconcile","cancel","snapshot","export"],
            "order_types":["Market","Limit"],"time_in_force":["Gtc"],"live":false,"amend":false,
            "fill_model":"snapshot-only, full marketable fills, flat configured quote fee"}),
        ),
        Command::Prepare { intent } => {
            runtime.prepare(*intent, now)?;
            Ok(json!({"prepared":true}))
        }
        Command::Abandon { client_order_id, reason } => {
            runtime.abandon(&client_order_id, &reason, now)?;
            Ok(json!({"abandoned":true}))
        }
        Command::Dispatch { client_order_id } => {
            runtime.dispatch(&client_order_id, venue, now)?;
            Ok(json!({"recorded":true}))
        }
        Command::Reconcile { client_order_id } => {
            runtime.reconcile(&client_order_id, venue, now)?;
            Ok(json!({"reconciled":true}))
        }
        Command::Cancel { client_order_id } => {
            runtime.cancel(&client_order_id, venue, now)?;
            Ok(json!({"recorded":true}))
        }
        Command::Snapshot => Ok(serde_json::to_value(runtime.snapshot()?)?),
        Command::Export => runtime.export(),
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        return Err(Error(
            "usage: replikan-trading CONFIG.json JOURNAL.sqlite PAPER_VENUE.sqlite".into(),
        ));
    }
    let config: Config = serde_json::from_reader(
        std::fs::File::open(&args[1]).map_err(|error| Error(error.to_string()))?,
    )?;
    let mut runtime = Runtime::open(&args[2], config.clone())?;
    let mut venue = PaperVenue::open(&args[3], config)?;
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    loop {
        // Read at most one MiB, rather than allocating an unbounded input line.
        let mut line = Vec::new();
        loop {
            let buffer = reader
                .fill_buf()
                .map_err(|error| Error(error.to_string()))?;
            if buffer.is_empty() {
                break;
            }
            let count = buffer
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(buffer.len(), |index| index + 1);
            if line.len() + count > 1_048_576 {
                return Err(Error("command exceeds input budget".into()));
            }
            let done = buffer[count - 1] == b'\n';
            line.extend_from_slice(&buffer[..count]);
            reader.consume(count);
            if done {
                break;
            }
        }
        if line.is_empty() {
            break;
        }
        let result = serde_json::from_slice::<Command>(&line)
            .map_err(Error::from)
            .and_then(|command| execute(&mut runtime, &mut venue, command));
        let response = match result {
            Ok(value) => json!({"ok":true,"result":value}),
            Err(error) => json!({"ok":false,"error":error.to_string()}),
        };
        writeln!(writer, "{response}").map_err(|error| Error(error.to_string()))?;
        writer.flush().map_err(|error| Error(error.to_string()))?;
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
