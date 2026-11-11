use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

fn field(line: &str, name: &str) -> Option<String> {
    let prefix = format!("\"{name}\":\"");
    line.split_once(&prefix)?.1.split_once('"').map(|(value, _)| value.to_owned())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let model = args.windows(2).find(|pair| pair[0] == "--model-dir").map(|pair| PathBuf::from(&pair[1])).unwrap();
    assert!(args.contains(&"--ipc-stdio".to_owned()));
    assert!(args.contains(&"--runtime-dir".to_owned()));
    let wrong_id = model.join("wrong-id").exists();
    let hang = model.join("hang-infer").exists();
    let crash = model.join("crash-infer").exists();
    let oversized = model.join("oversized-frame").exists();
    let mut loaded = false;
    let mut warmed = false;
    let mut calls = 0;
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        let id = field(&line, "id").unwrap();
        let command = field(&line, "command").unwrap();
        if oversized && command == "capabilities" {
            println!("{}", "X".repeat(1024 * 1024 + 16));
            io::stdout().flush().unwrap();
            continue;
        }
        if crash && command == "infer" { std::process::exit(71); }
        let result = match command.as_str() {
            "capabilities" => r#"{"model_id":"native-test","backends":["cpu"]}"#.to_string(),
            "load" => { loaded = true; r#"{"loaded":true}"#.to_string() },
            "warmup" => { assert!(loaded); warmed = true; r#"{"warm":true}"#.to_string() },
            "status" => format!(r#"{{"phase":"{}","model_id":"native-test"}}"#, if warmed {"ready"} else {"started"}),
            "infer" => { assert!(loaded && warmed); if hang { std::thread::sleep(Duration::from_millis(500)); } calls += 1; format!(r#"{{"sequence":{},"pid":{}}}"#, calls, std::process::id()) },
            "unload" => { loaded = false; warmed = false; r#"{}"#.to_string() },
            "shutdown" => r#"{}"#.to_string(),
            _ => panic!("unexpected command"),
        };
        let id = if wrong_id { "00000000-0000-0000-0000-000000000000" } else { &id };
        println!(r#"{{"version":1,"id":"{id}","ok":true,"result":{result},"error":null}}"#);
        io::stdout().flush().unwrap();
        if command == "shutdown" { break; }
    }
}
