use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn field(line: &str, name: &str) -> Option<String> {
    let prefix = format!("\"{name}\":\"");
    line.split_once(&prefix)?.1.split_once('"').map(|(value, _)| value.to_owned())
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let disagree = args.iter().any(|arg| arg == "--mismatched-speech");
    let runtime_dir = args.windows(2).find(|pair| pair[0] == "--runtime-dir")
        .map(|pair| PathBuf::from(&pair[1]));
    let mut loaded = false;
    let mut warmed = false;
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        let id = field(&line, "id").unwrap();
        let command = field(&line, "command").unwrap();
        let result = match command.as_str() {
            "capabilities" => {
                let version = if disagree { 2 } else { 1 };
                format!(r#"{{"model_id":"native-test","backends":["cpu"],"speech_contract_version":{},"speech_capabilities":[{{"kind":"tts","sample_rate_hz":16000,"languages":["ru"]}},{{"kind":"asr","sample_rate_hz":16000,"languages":["ru"]}}]}}"#, version)
            }
            "load" => { loaded = true; r#"{"loaded":true}"#.to_owned() }
            "warmup" => { assert!(loaded); warmed = true; r#"{"warm":true}"#.to_owned() }
            "status" => r#"{"phase":"ready","model_id":"native-test"}"#.to_owned(),
            "infer" => {
                assert!(loaded && warmed);
                match field(&line, "kind").as_deref() {
                    Some("tts") => {
                        if field(&line, "text").as_deref() == Some("hold-tts") {
                            let dir = runtime_dir.as_ref().expect("runtime directory");
                            std::fs::write(dir.join("tts-inflight"), b"started").unwrap();
                            let started = Instant::now();
                            while !dir.join("release-tts").exists() && started.elapsed() < Duration::from_secs(4) {
                                std::thread::sleep(Duration::from_millis(10));
                            }
                        }
                        r#"{"speech_version":1,"kind":"tts","audio_format":"wav","sample_rate_hz":16000,"audio_base64":"UklGRiYAAABXQVZFZm10IBAAAAABAAEAgD4AAAB9AAACABAAZGF0YQIAAAAAAA=="}"#.to_owned()
                    },
                    Some("asr") => r#"{"speech_version":1,"kind":"asr","text":"hello","language":"ru"}"#.to_owned(),
                    _ => panic!("unknown speech kind"),
                }
            }
            "unload" => { loaded = false; warmed = false; r#"{}"#.to_owned() }
            "shutdown" => r#"{}"#.to_owned(),
            _ => panic!("unsupported native command"),
        };
        println!(r#"{{"version":1,"id":"{id}","ok":true,"result":{result},"error":null}}"#);
        io::stdout().flush().unwrap();
        if command == "shutdown" { break; }
    }
}
