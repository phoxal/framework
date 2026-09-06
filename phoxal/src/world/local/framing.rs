use super::*;

pub(super) trait LocalQueryEndpoint: QueryEndpoint {
    const PATH: &'static str;
}

impl LocalQueryEndpoint for WorldSessionConnectRequest {
    const PATH: &'static str = CONNECT_PATH;
}

impl LocalQueryEndpoint for WorldSessionStateCurrentRequest {
    const PATH: &'static str = STATE_CURRENT_PATH;
}

impl LocalQueryEndpoint for WorldSessionDiagnosticsCurrentRequest {
    const PATH: &'static str = DIAGNOSTICS_CURRENT_PATH;
}

impl LocalQueryEndpoint for WorldSessionControlRequest {
    const PATH: &'static str = CONTROL_PATH;
}

pub(super) trait LocalSubscriptionEndpoint: Serialize {
    type Stream: DeserializeOwned + Send + 'static;
    const PATH: &'static str;
}

impl LocalSubscriptionEndpoint for WorldSessionStateSubscriptionRequest {
    type Stream = WorldSessionStateStream;
    const PATH: &'static str = STATE_PATH;
}

impl LocalSubscriptionEndpoint for WorldSessionDiagnosticsSubscriptionRequest {
    type Stream = WorldSessionDiagnosticsStream;
    const PATH: &'static str = DIAGNOSTICS_PATH;
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WireRequest {
    pub(super) path: String,
    pub(super) body: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WireReply {
    Value { body: Vec<u8> },
    Error { message: String },
    Timeout { operation: String, timeout_ms: u64 },
    Gap { stream: String, skipped: u64 },
}

pub(super) async fn with_timeout<T, F>(
    operation: &'static str,
    timeout: Duration,
    future: F,
) -> Result<T, WorldSessionWireError>
where
    F: Future<Output = Result<T, WorldSessionWireError>>,
{
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| WorldSessionWireError::Timeout {
            operation: operation.to_owned(),
            timeout_ms: timeout_millis(timeout),
        })?
}

fn timeout_millis(timeout: Duration) -> u64 {
    u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)
}

pub(super) async fn request<E: LocalQueryEndpoint>(
    endpoint: SocketAddr,
    request: &E,
) -> Result<E::Response, WorldSessionWireError> {
    let mut stream = with_timeout("connect", CONNECT_TIMEOUT, async move {
        Ok(TcpStream::connect(endpoint).await?)
    })
    .await?;
    with_timeout(
        "request write",
        FRAME_IO_TIMEOUT,
        write_frame(
            &mut stream,
            &WireRequest {
                path: E::PATH.to_owned(),
                body: rmp_serde::to_vec_named(request)?,
            },
        ),
    )
    .await?;
    let reply = with_timeout(
        "request response",
        CLIENT_OPERATION_TIMEOUT,
        read_frame(&mut stream),
    )
    .await?;
    decode_reply(reply)
}

pub(super) async fn open_subscription<E: LocalSubscriptionEndpoint>(
    endpoint: SocketAddr,
    request: &E,
) -> Result<WireSubscription<E::Stream>, WorldSessionWireError> {
    let mut stream = with_timeout("connect", CONNECT_TIMEOUT, async move {
        Ok(TcpStream::connect(endpoint).await?)
    })
    .await?;
    with_timeout(
        "subscription request write",
        FRAME_IO_TIMEOUT,
        write_frame(
            &mut stream,
            &WireRequest {
                path: E::PATH.to_owned(),
                body: rmp_serde::to_vec_named(request)?,
            },
        ),
    )
    .await?;
    let reply = with_timeout(
        "subscription handshake",
        FRAME_IO_TIMEOUT,
        read_frame::<_, WireReply>(&mut stream),
    )
    .await?;
    let initial = decode_reply(reply)?;
    let (sender, receiver) = mpsc::channel(CLIENT_STREAM_CAPACITY);
    sender.try_send(Ok(initial)).map_err(|_| {
        WorldSessionWireError::Protocol("subscription bootstrap queue closed".to_owned())
    })?;
    let task = tokio::spawn(async move {
        loop {
            let value = match read_frame::<_, WireReply>(&mut stream).await {
                Ok(reply) => decode_reply(reply),
                Err(error) => Err(error),
            };
            let terminal = value.is_err();
            if sender.send(value).await.is_err() || terminal {
                return;
            }
        }
    });
    Ok(WireSubscription { receiver, task })
}

pub(super) async fn send_value<W: AsyncWrite + Unpin, T: Serialize>(
    stream: &mut W,
    value: &T,
) -> Result<(), WorldSessionWireError> {
    with_timeout(
        "response write",
        FRAME_IO_TIMEOUT,
        write_frame(
            stream,
            &WireReply::Value {
                body: rmp_serde::to_vec_named(value)?,
            },
        ),
    )
    .await
}

pub(super) async fn send_error<W: AsyncWrite + Unpin>(
    stream: &mut W,
    message: String,
) -> Result<(), WorldSessionWireError> {
    with_timeout(
        "error response write",
        FRAME_IO_TIMEOUT,
        write_frame(stream, &WireReply::Error { message }),
    )
    .await
}

pub(super) async fn send_timeout<W: AsyncWrite + Unpin>(
    stream: &mut W,
    operation: &'static str,
    timeout: Duration,
) -> Result<(), WorldSessionWireError> {
    with_timeout(
        "timeout response write",
        FRAME_IO_TIMEOUT,
        write_frame(
            stream,
            &WireReply::Timeout {
                operation: operation.to_owned(),
                timeout_ms: timeout_millis(timeout),
            },
        ),
    )
    .await
}

pub(super) async fn send_gap<W: AsyncWrite + Unpin>(
    stream: &mut W,
    stream_name: &'static str,
    skipped: u64,
) -> Result<(), WorldSessionWireError> {
    with_timeout(
        "stream gap response write",
        FRAME_IO_TIMEOUT,
        write_frame(
            stream,
            &WireReply::Gap {
                stream: stream_name.to_owned(),
                skipped,
            },
        ),
    )
    .await
}

fn decode_reply<T: DeserializeOwned>(reply: WireReply) -> Result<T, WorldSessionWireError> {
    match reply {
        WireReply::Value { body } => Ok(rmp_serde::from_slice(&body)?),
        WireReply::Error { message } => Err(WorldSessionWireError::Refused(message)),
        WireReply::Timeout {
            operation,
            timeout_ms,
        } => Err(WorldSessionWireError::Timeout {
            operation,
            timeout_ms,
        }),
        WireReply::Gap { stream, skipped } => {
            let stream = match stream.as_str() {
                "state" => "state",
                "diagnostics" => "diagnostics",
                _ => {
                    return Err(WorldSessionWireError::Protocol(format!(
                        "world host reported a gap for unknown stream '{stream}'"
                    )));
                }
            };
            Err(WorldSessionWireError::StreamGap { stream, skipped })
        }
    }
}

pub(super) fn decode_body<T: DeserializeOwned>(body: &[u8]) -> Result<T, WorldSessionWireError> {
    Ok(rmp_serde::from_slice(body)?)
}

pub(super) async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), WorldSessionWireError> {
    let body = rmp_serde::to_vec_named(value)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(WorldSessionWireError::FrameTooLarge {
            bytes: body.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }
    let length = u32::try_from(body.len()).map_err(|_| WorldSessionWireError::FrameTooLarge {
        bytes: body.len(),
        maximum: MAX_FRAME_BYTES,
    })?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

pub(super) async fn read_frame<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> Result<T, WorldSessionWireError> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(WorldSessionWireError::FrameTooLarge {
            bytes: length,
            maximum: MAX_FRAME_BYTES,
        });
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).await?;
    Ok(rmp_serde::from_slice(&body)?)
}

pub(super) fn parse_endpoint(endpoint: &str) -> Result<SocketAddr, WorldSessionWireError> {
    let address =
        endpoint
            .strip_prefix("tcp://")
            .ok_or_else(|| WorldSessionWireError::InvalidEndpoint {
                endpoint: endpoint.to_owned(),
            })?;
    let address =
        address
            .parse::<SocketAddr>()
            .map_err(|_| WorldSessionWireError::InvalidEndpoint {
                endpoint: endpoint.to_owned(),
            })?;
    if !address.ip().is_loopback() {
        return Err(WorldSessionWireError::InvalidEndpoint {
            endpoint: endpoint.to_owned(),
        });
    }
    Ok(address)
}
