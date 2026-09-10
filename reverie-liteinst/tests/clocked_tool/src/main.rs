use std::sync::Arc;

fn main() {
    let socket = std::env::args_os().nth(1).expect("socket path");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap();
    runtime.block_on(async {
        let server = reverie_rpc_transport::RpcServer::bind(
            std::path::Path::new(&socket),
            Arc::new(liteinst_clocked_tool_fixture::ClockGlobal),
            (),
        )
        .unwrap();
        server.serve().await.unwrap();
    });
}
