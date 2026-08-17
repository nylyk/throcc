use quinn::Connection;

pub async fn serve(connection: Connection) {
    tracing::info!(
        rtt = ?connection.rtt(),
        max_datagram = ?connection.max_datagram_size(),
        "connected"
    );

    let reason = connection.closed().await;
    tracing::info!(%reason, "disconnected");
}
