use anyhow::Result;

fn main() -> Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:9001".to_string());
    signaling_server::serve(&addr)
}
