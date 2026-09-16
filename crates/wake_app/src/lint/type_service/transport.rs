//! Bounded stdio JSON-RPC transport for one owned native TypeScript process.
use crate::{CancellationToken, WakeError};
use serde_json::{Value, json};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub frame: usize,
    pub header: usize,
    pub bytes: usize,
    pub messages: usize,
    pub timeout: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            frame: 32 * 1024 * 1024,
            header: 8192,
            bytes: 256 * 1024 * 1024,
            messages: 200_000,
            timeout: Duration::from_secs(120),
        }
    }
}

fn failure(message: impl std::fmt::Display) -> WakeError {
    WakeError::new("WAKE_LINT_ANALYSIS", format!("Type service: {message}"))
}

#[derive(Debug)]
struct Frame {
    payload: Vec<u8>,
    bytes: usize,
}

fn read_frame(reader: &mut impl BufRead, limits: Limits) -> io::Result<Frame> {
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidData, message);
    let mut header = 0usize;
    let mut length = None;
    loop {
        let mut line = Vec::new();
        let read = reader
            .take(limits.header.saturating_sub(header) as u64 + 1)
            .read_until(b'\n', &mut line)?;
        header += read;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "type service closed stdout",
            ));
        }
        if header > limits.header {
            return Err(invalid("header budget exceeded"));
        }
        let line = line
            .strip_suffix(b"\r\n")
            .ok_or_else(|| invalid("invalid header delimiter"))?;
        if line.is_empty() {
            break;
        }
        let line = std::str::from_utf8(line).map_err(|_| invalid("invalid header text"))?;
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| invalid("invalid header field"))?;
        if !name.eq_ignore_ascii_case("Content-Length") {
            return Err(invalid("unknown header field"));
        }
        let value = value.trim();
        if length.is_some() || value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(invalid("invalid or repeated Content-Length"));
        }
        let value: usize = value
            .parse()
            .map_err(|_| invalid("invalid Content-Length"))?;
        if value > limits.frame {
            return Err(invalid("frame budget exceeded"));
        }
        length = Some(value);
    }
    let length = length.ok_or_else(|| invalid("missing Content-Length"))?;
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    Ok(Frame {
        payload,
        bytes: header + length,
    })
}

fn encode(value: &Value, limits: Limits) -> Result<Vec<u8>, WakeError> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(io::Error::other("frame budget exceeded"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut body = Bounded {
        bytes: Vec::new(),
        limit: limits.frame,
    };
    serde_json::to_writer(&mut body, value).map_err(failure)?;
    let mut output = format!("Content-Length: {}\r\n\r\n", body.bytes.len()).into_bytes();
    output.extend(body.bytes);
    Ok(output)
}

fn decode(bytes: &[u8]) -> Result<Value, WakeError> {
    struct Envelope;
    impl<'de> serde::de::Visitor<'de> for Envelope {
        type Value = Value;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a closed JSON-RPC envelope")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
            let mut fields = serde_json::Map::new();
            while let Some(key) = access.next_key::<String>()? {
                if !matches!(
                    key.as_str(),
                    "jsonrpc" | "id" | "method" | "params" | "result" | "error"
                ) || fields.contains_key(&key)
                {
                    return Err(serde::de::Error::custom(
                        "duplicate or unknown JSON-RPC field",
                    ));
                }
                fields.insert(key, access.next_value::<Value>()?);
            }
            if fields.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                return Err(serde::de::Error::custom("invalid JSON-RPC version"));
            }
            Ok(Value::Object(fields))
        }
    }
    use serde::de::Deserializer as _;
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = (&mut deserializer)
        .deserialize_map(Envelope)
        .map_err(failure)?;
    deserializer.end().map_err(failure)?;
    Ok(value)
}

pub(super) struct Session {
    child: Child,
    outgoing: Option<SyncSender<Vec<u8>>>,
    incoming: Option<Receiver<Result<Frame, WakeError>>>,
    workers: Vec<JoinHandle<()>>,
    cancellation: CancellationToken,
    deadline: Instant,
    limits: Limits,
    bytes: usize,
    messages: usize,
    next_id: u64,
}

impl Session {
    pub(super) fn spawn(
        executable: &Path,
        root: &Path,
        cancellation: CancellationToken,
        limits: Limits,
    ) -> Result<Self, WakeError> {
        cancellation.check()?;
        let mut command = Command::new(executable);
        command
            .args(["--api", "--async", "--cwd"])
            .arg(root)
            .arg("--callbacks=readFile,fileExists,directoryExists,getAccessibleEntries,realpath")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(failure)?;
        let mut input = child.stdin.take().expect("configured stdin pipe");
        let output = child.stdout.take().expect("configured stdout pipe");
        let mut stderr = child.stderr.take().expect("configured stderr pipe");
        let (send, writes) = mpsc::sync_channel::<Vec<u8>>(1);
        let (events, receive) = mpsc::sync_channel(1);
        let mut session = Self {
            child,
            outgoing: Some(send),
            incoming: Some(receive),
            workers: Vec::new(),
            cancellation,
            deadline: Instant::now() + limits.timeout,
            limits,
            bytes: 0,
            messages: 0,
            next_id: 0,
        };
        let writer_events = events.clone();
        session.workers.push(
            thread::Builder::new()
                .name("wake-lint-type-write".into())
                .spawn(move || {
                    while let Ok(bytes) = writes.recv() {
                        if let Err(error) = input.write_all(&bytes).and_then(|()| input.flush()) {
                            let _ = writer_events.send(Err(failure(error)));
                            break;
                        }
                    }
                })
                .map_err(failure)?,
        );
        session.workers.push(
            thread::Builder::new()
                .name("wake-lint-type-read".into())
                .spawn(move || {
                    let mut reader = BufReader::new(output);
                    loop {
                        let frame = read_frame(&mut reader, limits).map_err(failure);
                        let failed = frame.is_err();
                        if events.send(frame).is_err() || failed {
                            break;
                        }
                    }
                })
                .map_err(failure)?,
        );
        // Drain stderr with constant memory so an erroring backend cannot block stdout progress.
        session.workers.push(
            thread::Builder::new()
                .name("wake-lint-type-stderr".into())
                .spawn(move || {
                    let mut bytes = [0; 8192];
                    loop {
                        match stderr.read(&mut bytes) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                })
                .map_err(failure)?,
        );
        Ok(session)
    }

    fn check(&self) -> Result<(), WakeError> {
        self.cancellation.check()?;
        if Instant::now() >= self.deadline {
            return Err(failure("session deadline exceeded"));
        }
        if self.outgoing.is_none() {
            return Err(failure("session is closed"));
        }
        Ok(())
    }

    fn account(&mut self, bytes: usize) -> Result<(), WakeError> {
        if bytes > self.limits.bytes.saturating_sub(self.bytes)
            || self.messages >= self.limits.messages
        {
            return Err(failure("session transfer budget exceeded"));
        }
        self.bytes += bytes;
        self.messages += 1;
        Ok(())
    }

    fn send(&mut self, value: &Value) -> Result<(), WakeError> {
        self.check()?;
        let mut bytes = encode(value, self.limits)?;
        self.account(bytes.len())?;
        loop {
            self.check()?;
            match self
                .outgoing
                .as_ref()
                .expect("open session")
                .try_send(bytes)
            {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(pending)) => {
                    bytes = pending;
                    thread::sleep(Duration::from_millis(10));
                }
                Err(TrySendError::Disconnected(_)) => return Err(failure("writer exited")),
            }
        }
    }

    fn receive(&mut self) -> Result<Value, WakeError> {
        loop {
            self.check()?;
            match self
                .incoming
                .as_ref()
                .expect("open session")
                .recv_timeout(Duration::from_millis(10))
            {
                Ok(frame) => {
                    let frame = frame?;
                    self.account(frame.bytes)?;
                    return decode(&frame.payload);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(failure("reader exited")),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(status) = self.child.try_wait().map_err(failure)? {
                        return Err(failure(format!("backend exited: {status}")));
                    }
                }
            }
        }
    }

    pub(super) fn request(
        &mut self,
        method: &str,
        params: Value,
        mut callback: impl FnMut(&str, &Value) -> Result<Value, WakeError>,
    ) -> Result<Value, WakeError> {
        let result = self.request_inner(method, params, &mut callback);
        if result.is_err() {
            self.shutdown();
        }
        result
    }

    fn request_inner(
        &mut self,
        method: &str,
        params: Value,
        callback: &mut impl FnMut(&str, &Value) -> Result<Value, WakeError>,
    ) -> Result<Value, WakeError> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))?;
        loop {
            let message = self.receive()?;
            if let Some(method) = message.get("method") {
                let method = method
                    .as_str()
                    .ok_or_else(|| failure("invalid callback method"))?;
                if !matches!(
                    method,
                    "readFile"
                        | "fileExists"
                        | "directoryExists"
                        | "getAccessibleEntries"
                        | "realpath"
                ) {
                    return Err(failure(format!("unsupported callback: {method}")));
                }
                if !(message["id"].is_u64() || message["id"].is_string())
                    || !message["params"].is_string()
                    || message.get("result").is_some()
                    || message.get("error").is_some()
                {
                    return Err(failure("invalid filesystem callback"));
                }
                let result = callback(method, &message["params"])?;
                self.send(&json!({"jsonrpc":"2.0", "id":message["id"], "result":result}))?;
            } else {
                if message["id"].as_u64() != Some(id)
                    || message.get("result").is_some() == message.get("error").is_some()
                {
                    return Err(failure("unexpected response identity or payload"));
                }
                if let Some(error) = message.get("error") {
                    return Err(failure(format!("backend request failed: {error}")));
                }
                return Ok(message["result"].clone());
            }
        }
    }

    pub(super) fn shutdown(&mut self) {
        self.outgoing.take();
        self.incoming.take();
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn envelopes_reject_duplicate_or_unknown_fields_without_losing_null_results() {
        for bytes in [
            br#"{"jsonrpc":"2.0","id":1,"id":2,"result":null}"#.as_slice(),
            br#"{"jsonrpc":"2.0","id":1,"result":null,"extra":true}"#,
            br#"{"jsonrpc":"1.0","jsonrpc":"2.0","id":1,"result":null}"#,
            br#"{"jsonrpc":"2.0","id":1,"result":null} {}"#,
        ] {
            assert!(super::decode(bytes).is_err());
        }
        let decoded = super::decode(br#"{"jsonrpc":"2.0","id":1,"result":null}"#).unwrap();
        assert!(
            decoded
                .get("result")
                .is_some_and(serde_json::Value::is_null)
        );
    }

    use super::*;
    use std::io::Cursor;

    #[test]
    fn framing_rejects_truncation_duplicate_lengths_and_unbounded_input() {
        let limits = Limits {
            header: 80,
            frame: 32,
            ..Limits::default()
        };
        let valid = read_frame(&mut Cursor::new(b"Content-Length: 2\r\n\r\n{}"), limits).unwrap();
        assert_eq!(valid.payload, b"{}");
        assert_eq!(valid.bytes, 23);
        for bad in [
            "Content-Length: 33\r\n\r\n",
            "Content-Length: -1\r\n\r\n",
            "Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
            "Content-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
            "Content-Length: 2\r\nX-Test: rejected\r\n\r\n{}",
            "Content-Length: 3\r\n\r\n{}",
            "Content-Length: 2\n\n{}",
            "Other: 2\r\n\r\n{}",
        ] {
            assert!(
                read_frame(&mut Cursor::new(bad), limits).is_err(),
                "{bad:?}"
            );
        }
        assert!(read_frame(&mut Cursor::new("x".repeat(100)), limits).is_err());
        assert!(encode(&serde_json::json!({"tooBig":"x".repeat(40)}), limits).is_err());
    }

    #[test]
    #[ignore = "requires WAKE_LINT_TYPESCRIPT_EXE pointing to the installed native TypeScript 7.0.2"]
    fn native_stdio_queries_snapshots_and_reaps_the_owned_process_on_timeout_and_cancel() {
        use crate::CancellationToken;
        use std::time::Duration;
        let executable = std::env::var_os("WAKE_LINT_TYPESCRIPT_EXE")
            .expect("explicit native TypeScript fixture");
        let root = tempfile::tempdir().unwrap();
        let cancellation = CancellationToken::default();
        let mut service = Session::spawn(
            std::path::Path::new(&executable),
            root.path(),
            cancellation.clone(),
            Limits::default(),
        )
        .unwrap();
        let initialized = service
            .request("initialize", serde_json::Value::Null, |_, _| {
                panic!("initialize must not query filesystem")
            })
            .unwrap();
        assert!(initialized["useCaseSensitiveFileNames"].is_boolean());
        let config = root
            .path()
            .join("tsconfig.json")
            .to_string_lossy()
            .replace('\\', "/");
        let source = root
            .path()
            .join("a.ts")
            .to_string_lossy()
            .replace('\\', "/");
        let source_text = "const value = 42; /* 😀 */ function read() { return value; } read();";
        let files = std::collections::BTreeMap::from([
            (
                config.clone(),
                "{\"compilerOptions\":{\"strict\":true,\"noLib\":true},\"files\":[\"a.ts\"]}"
                    .to_owned(),
            ),
            (source.clone(), source_text.to_owned()),
        ]);
        let mut callbacks = 0;
        let snapshot = service
            .request(
                "updateSnapshot",
                serde_json::json!({"openProjects":[config]}),
                |method, path| {
                    callbacks += 1;
                    let path = path.as_str().unwrap().replace('\\', "/");
                    Ok(match method {
                        "readFile" => serde_json::json!({"content":files.get(&path)}),
                        "fileExists" => serde_json::json!(files.contains_key(&path)),
                        "directoryExists" => {
                            serde_json::json!(std::path::Path::new(&path).is_dir())
                        }
                        "getAccessibleEntries" => serde_json::json!({"files":[],"directories":[]}),
                        "realpath" => serde_json::json!(path),
                        _ => panic!("unexpected callback {method}"),
                    })
                },
            )
            .unwrap();
        assert!(callbacks > 0);
        let project = snapshot["projects"][0]["id"].clone();
        let snapshot = snapshot["snapshot"].clone();
        let typed = service.request("getTypeAtPosition", serde_json::json!({"snapshot":snapshot,"project":project,"file":source,"position":6}), |_, _| panic!("snapshot must retain source")).unwrap();
        assert!(typed["id"].is_u64(), "{typed}");
        let encoded = service
            .request(
                "getSourceFile",
                json!({"snapshot":snapshot,"project":project,"file":source}),
                |_, _| panic!("snapshot must retain source"),
            )
            .unwrap();
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded["data"].as_str().unwrap())
            .unwrap();
        let wire = super::super::wire::WireSource::parse(
            &bytes,
            source_text,
            Path::new(&source),
            initialized["useCaseSensitiveFileNames"].as_bool().unwrap(),
        )
        .unwrap();
        let span = wake_common::Span::new(
            source_text.rfind("read()").unwrap() as u32,
            (source_text.len() - 1) as u32,
        );
        let address = wire.address(214, span).unwrap();
        let call_type = service
            .request(
                "getTypeAtLocation",
                json!({"snapshot":snapshot,"project":project,"location":address}),
                |_, _| panic!("snapshot must retain source"),
            )
            .unwrap();
        assert_eq!(
            service
                .request(
                    "typeToString",
                    json!({"snapshot":snapshot,"project":project,"type":call_type["id"]}),
                    |_, _| unreachable!()
                )
                .unwrap(),
            "number"
        );
        cancellation.cancel();
        assert_eq!(
            service
                .request("initialize", serde_json::Value::Null, |_, _| unreachable!())
                .unwrap_err()
                .code,
            "WAKE_CANCELLED"
        );
        service.shutdown();
        assert!(service.child.try_wait().unwrap().is_some());
        let mut stalled = Session::spawn(
            std::path::Path::new(&executable),
            root.path(),
            CancellationToken::default(),
            Limits {
                timeout: Duration::from_millis(40),
                ..Limits::default()
            },
        )
        .unwrap();
        let error = stalled.receive().unwrap_err();
        assert!(error.message.contains("deadline"), "{error}");
        stalled.shutdown();
        assert!(stalled.child.try_wait().unwrap().is_some());

        for message in [
            json!({"jsonrpc":"2.0", "id":"callback", "method":"writeFile", "params":"source.ts"}),
            json!({"jsonrpc":"2.0", "id":99, "result":{}}),
            json!({"jsonrpc":"1.0", "id":1, "result":{}}),
            json!({"jsonrpc":"2.0", "id":1, "result":{}, "error":{}}),
            json!({"jsonrpc":"2.0", "id":"callback", "method":"readFile", "params":{}}),
        ] {
            let mut bad = Session::spawn(
                Path::new(&executable),
                root.path(),
                CancellationToken::default(),
                Limits::default(),
            )
            .unwrap();
            let (sender, receiver) = mpsc::sync_channel(1);
            bad.incoming = Some(receiver);
            let payload = serde_json::to_vec(&message).unwrap();
            sender
                .send(Ok(Frame {
                    bytes: payload.len(),
                    payload,
                }))
                .unwrap();
            assert_eq!(
                bad.request("initialize", Value::Null, |_, _| panic!(
                    "invalid callback reached the filesystem"
                ))
                .unwrap_err()
                .code,
                "WAKE_LINT_ANALYSIS"
            );
            assert!(bad.child.try_wait().unwrap().is_some());
        }
        for limits in [
            Limits {
                bytes: 0,
                ..Limits::default()
            },
            Limits {
                messages: 0,
                ..Limits::default()
            },
        ] {
            let mut limited = Session::spawn(
                Path::new(&executable),
                root.path(),
                CancellationToken::default(),
                limits,
            )
            .unwrap();
            assert!(
                limited
                    .request("initialize", Value::Null, |_, _| unreachable!())
                    .unwrap_err()
                    .message
                    .contains("transfer budget")
            );
            assert!(limited.child.try_wait().unwrap().is_some());
        }
    }
}
