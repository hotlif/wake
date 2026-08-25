//! Versioned test-host wire protocol.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{TestCaseResult, TestDiagnostic, TestOptions, TestRunResult, TestSuiteResult};

/// The protocol is intentionally incompatible with the former one-request-per-connection wire.
pub const PROTOCOL_VERSION: u32 = 3;
/// Identifies the host implementation paired with this Wake Test product build.
pub const HOST_BUILD_ID: &str = concat!("wake-test-host/", env!("CARGO_PKG_VERSION"));
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Incrementally decodes frames without losing a partial TCP read across timeouts.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    pub fn decode_next<T: DeserializeOwned>(&mut self) -> std::io::Result<Option<T>> {
        if self.buffer.len() < 4 {
            return Ok(None);
        }
        let length = u32::from_be_bytes(
            self.buffer[..4]
                .try_into()
                .expect("four prefix bytes were checked"),
        ) as usize;
        if length > MAX_FRAME_BYTES {
            return Err(frame_limit_error());
        }
        let frame_length = length.checked_add(4).ok_or_else(frame_limit_error)?;
        if self.buffer.len() < frame_length {
            return Ok(None);
        }
        let value = serde_json::from_slice(&self.buffer[4..frame_length])
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        self.buffer.drain(..frame_length);
        Ok(Some(value))
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostHello {
    pub protocol_version: u32,
    pub build_id: String,
    pub address: String,
    pub process_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostRequest {
    pub protocol_version: u32,
    pub build_id: String,
    /// The random launch token is repeated and verified on every request frame.
    pub token: String,
    pub request_id: u64,
    pub command: HostCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HostCommand {
    Run {
        run_id: String,
        options: Box<TestOptions>,
    },
    Cancel {
        run_id: String,
    },
    StartWatch {
        watch_id: String,
        options: Box<TestOptions>,
    },
    StopWatch {
        watch_id: String,
    },
    WatchControl {
        watch_id: String,
        control: WatchControl,
    },
    Shutdown,
}

/// Wake-owned interactive watch controls. These are intentionally not Jest key/API aliases.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WatchControl {
    All,
    Failed,
    Path { pattern: String },
    Name { pattern: String },
    UpdateSnapshots,
    Rerun,
}

/// A response frame. `sequence` is monotonically increasing within one TCP session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostResponse {
    pub protocol_version: u32,
    pub build_id: String,
    pub request_id: u64,
    pub sequence: u64,
    pub body: HostResponseBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HostResponseBody {
    Ack {
        command: HostAck,
    },
    Event {
        event: Box<HostEvent>,
    },
    Result {
        run_id: String,
        result: Box<TestRunResult>,
    },
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        error: HostError,
    },
    /// Internal watch terminal. It is never exposed as a synthetic public result/event.
    WatchRunError {
        watch_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        started: bool,
        error: HostError,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HostAck {
    Run { run_id: String },
    Cancel { run_id: String },
    StartWatch { watch_id: String },
    StopWatch { watch_id: String },
    WatchControl { watch_id: String },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HostEvent {
    RunStart {
        run_id: String,
        watching: bool,
    },
    TestCaseResult {
        run_id: String,
        suite_id: String,
        result: Box<TestCaseResult>,
    },
    SuiteResult {
        run_id: String,
        result: Box<TestSuiteResult>,
    },
    Diagnostic {
        #[serde(skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        diagnostic: Box<TestDiagnostic>,
    },
    WatchReady {
        watch_id: String,
        root: String,
    },
    RunComplete {
        watch_id: String,
        run_id: String,
        result: Box<TestRunResult>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostError {
    pub code: String,
    pub message: String,
    pub path: Option<String>,
}

pub fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> std::io::Result<()> {
    let payload = serde_json::to_vec(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(frame_limit_error());
    }
    let length = u32::try_from(payload.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "test-host frame length does not fit u32",
        )
    })?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()
}

pub fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> std::io::Result<T> {
    read_optional_frame(reader)?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "test-host session ended before the next frame",
        )
    })
}

/// Reads a frame while distinguishing a clean session EOF from a truncated frame.
pub fn read_optional_frame<T: DeserializeOwned>(
    reader: &mut impl Read,
) -> std::io::Result<Option<T>> {
    let mut length = [0_u8; 4];
    loop {
        match reader.read(&mut length[..1]) {
            Ok(0) => return Ok(None),
            Ok(1) => break,
            Ok(_) => unreachable!("the read buffer contains one byte"),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    reader.read_exact(&mut length[1..])?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(frame_limit_error());
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn frame_limit_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "test-host frame exceeds the protocol limit",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(request_id: u64, command: HostCommand) -> HostRequest {
        HostRequest {
            protocol_version: PROTOCOL_VERSION,
            build_id: HOST_BUILD_ID.to_string(),
            token: "secret".to_string(),
            request_id,
            command,
        }
    }

    #[test]
    fn frame_round_trip_is_length_prefixed_json() {
        let request = request(
            7,
            HostCommand::StartWatch {
                watch_id: "watch-1".to_string(),
                options: Box::new(TestOptions::default()),
            },
        );
        let mut frame = Vec::new();
        write_frame(&mut frame, &request).unwrap();
        assert_eq!(
            u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize,
            frame.len() - 4
        );
        let decoded: HostRequest = read_frame(&mut frame.as_slice()).unwrap();
        assert_eq!(decoded.request_id, 7);
        assert_eq!(decoded.build_id, HOST_BUILD_ID);
        assert!(matches!(
            decoded.command,
            HostCommand::StartWatch { watch_id, .. } if watch_id == "watch-1"
        ));
    }

    #[test]
    fn optional_reader_supports_multiple_frames_and_clean_eof() {
        let mut frames = Vec::new();
        write_frame(
            &mut frames,
            &request(
                1,
                HostCommand::Cancel {
                    run_id: "run-1".to_string(),
                },
            ),
        )
        .unwrap();
        write_frame(&mut frames, &request(2, HostCommand::Shutdown)).unwrap();

        let mut input = frames.as_slice();
        let first = read_optional_frame::<HostRequest>(&mut input)
            .unwrap()
            .unwrap();
        let second = read_optional_frame::<HostRequest>(&mut input)
            .unwrap()
            .unwrap();
        assert_eq!(first.request_id, 1);
        assert_eq!(second.request_id, 2);
        assert!(
            read_optional_frame::<HostRequest>(&mut input)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn incremental_decoder_preserves_fragmented_and_coalesced_frames() {
        let mut frames = Vec::new();
        write_frame(&mut frames, &request(1, HostCommand::Shutdown)).unwrap();
        write_frame(&mut frames, &request(2, HostCommand::Shutdown)).unwrap();

        let split = 7;
        let mut decoder = FrameDecoder::new();
        decoder.push(&frames[..split]);
        assert!(decoder.decode_next::<HostRequest>().unwrap().is_none());
        decoder.push(&frames[split..]);
        assert_eq!(
            decoder
                .decode_next::<HostRequest>()
                .unwrap()
                .unwrap()
                .request_id,
            1
        );
        assert_eq!(
            decoder
                .decode_next::<HostRequest>()
                .unwrap()
                .unwrap()
                .request_id,
            2
        );
        assert!(decoder.decode_next::<HostRequest>().unwrap().is_none());
        assert!(decoder.is_empty());
    }

    #[test]
    fn response_body_is_a_tagged_ordered_event() {
        let response = HostResponse {
            protocol_version: PROTOCOL_VERSION,
            build_id: HOST_BUILD_ID.to_string(),
            request_id: 9,
            sequence: 3,
            body: HostResponseBody::Event {
                event: Box::new(HostEvent::RunStart {
                    run_id: "run-9".to_string(),
                    watching: true,
                }),
            },
        };
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "buildId": HOST_BUILD_ID,
                "requestId": 9,
                "sequence": 3,
                "body": {
                    "type": "event",
                    "event": {
                        "type": "runStart",
                        "runId": "run-9",
                        "watching": true
                    }
                }
            })
        );
    }

    #[test]
    fn enum_variant_fields_have_one_camel_case_v3_wire_shape() {
        let command = HostCommand::WatchControl {
            watch_id: "watch-1".to_string(),
            control: WatchControl::Path {
                pattern: "src/button".to_string(),
            },
        };
        let value = serde_json::to_value(&command).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "type": "watchControl",
                "watchId": "watch-1",
                "control": {"type": "path", "pattern": "src/button"}
            })
        );
        assert!(matches!(
            serde_json::from_value::<HostCommand>(value).unwrap(),
            HostCommand::WatchControl { watch_id, control: WatchControl::Path { pattern } }
                if watch_id == "watch-1" && pattern == "src/button"
        ));

        assert_eq!(
            serde_json::to_value(HostAck::Run {
                run_id: "run-1".to_string()
            })
            .unwrap(),
            serde_json::json!({"type": "run", "runId": "run-1"})
        );
    }

    #[test]
    fn watch_run_error_preserves_started_state_without_fabricating_a_result() {
        let value = serde_json::to_value(HostResponseBody::WatchRunError {
            watch_id: "watch-1".to_string(),
            run_id: Some("watch-1-run-2".to_string()),
            started: true,
            error: HostError {
                code: "WAKE_TEST_HOST".to_string(),
                message: "worker crashed".to_string(),
                path: None,
            },
        })
        .unwrap();
        assert_eq!(value["type"], "watchRunError");
        assert_eq!(value["watchId"], "watch-1");
        assert_eq!(value["runId"], "watch-1-run-2");
        assert_eq!(value["started"], true);
        assert!(value.get("result").is_none());
    }

    #[test]
    fn watch_controls_use_the_wake_v3_tagged_contract() {
        let controls = [
            (WatchControl::All, "all", None),
            (WatchControl::Failed, "failed", None),
            (
                WatchControl::Path {
                    pattern: "src/button".to_string(),
                },
                "path",
                Some("src/button"),
            ),
            (
                WatchControl::Name {
                    pattern: "renders".to_string(),
                },
                "name",
                Some("renders"),
            ),
            (WatchControl::UpdateSnapshots, "updateSnapshots", None),
            (WatchControl::Rerun, "rerun", None),
        ];

        for (control, tag, pattern) in controls {
            let value = serde_json::to_value(HostCommand::WatchControl {
                watch_id: "watch-1".to_string(),
                control,
            })
            .unwrap();
            assert_eq!(value["type"], "watchControl");
            assert_eq!(value["control"]["type"], tag);
            if let Some(pattern) = pattern {
                assert_eq!(value["control"]["pattern"], pattern);
            }
            serde_json::from_value::<HostCommand>(value).unwrap();
        }
    }

    #[test]
    fn oversized_frame_is_rejected_before_allocation() {
        let length = ((MAX_FRAME_BYTES as u32) + 1).to_be_bytes();
        let mut frame = length.as_slice();
        let error = read_frame::<HostRequest>(&mut frame).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }
}
