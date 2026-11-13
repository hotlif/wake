use super::*;
use std::io::{Read, Write};

fn rule(address: std::net::SocketAddr, timeout: Option<u64>) -> CompiledProxy {
    CompiledProxy::compile(ProxyRule {
        context: vec!["/api".into()],
        target: format!("http://{address}"),
        ws: false,
        change_origin: true,
        path_rewrite: vec![],
        connect_timeout_ms: Some(500),
        request_timeout_ms: timeout,
    })
    .unwrap()
}

#[test]
fn proxy_timeout_does_not_replay_a_write() {
    actix_web::rt::System::new().block_on(async {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let backend = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            assert!(request.starts_with(b"POST "));
            std::thread::sleep(Duration::from_millis(400));
            // The backend can commit even after the proxy has timed out.
            let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        });
        let request = actix_web::test::TestRequest::post()
            .uri("/api/write")
            .to_http_request();
        let response = forward(&request, web::Bytes::new(), &rule(address, Some(150))).await;
        assert_eq!(response.status(), actix_web::http::StatusCode::BAD_GATEWAY);
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains("上游执行结果可能未知"));
        backend.join().unwrap();
    });
}

#[test]
fn proxy_unreachable_upstream_is_bounded_without_request_timeout() {
    actix_web::rt::System::new().block_on(async {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let request = actix_web::test::TestRequest::get()
            .uri("/api")
            .to_http_request();
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            forward(&request, web::Bytes::new(), &rule(address, None)),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), actix_web::http::StatusCode::BAD_GATEWAY);
    });
}

#[test]
fn proxy_cancelled_response_releases_upstream() {
    actix_web::rt::System::new().block_on(async {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let backend = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
            let mut byte = [0];
            match socket.read(&mut byte) {
                Ok(0) => {},
                Err(error) if matches!(error.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted) => {},
                other => panic!("upstream must close on cancellation: {other:?}"),
            }
        });
        let request = actix_web::test::TestRequest::get().uri("/api").to_http_request();
        let response = forward(&request, web::Bytes::new(), &rule(address, None)).await;
        assert_eq!(response.status(), actix_web::http::StatusCode::OK);
        drop(response);
        tokio::task::spawn_blocking(move || backend.join().unwrap()).await.unwrap();
    });
}
