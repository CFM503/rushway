from pathlib import Path
import re


def sub(path: str, pattern: str, repl: str, flags: int = re.M) -> int:
    p = Path(path)
    s = p.read_text()
    out, count = re.subn(pattern, repl, s, flags=flags)
    if count:
        p.write_text(out)
    print(f"{path}: {count} replacements: {pattern}")
    return count


sub("src/mux_pool.rs", r"(?m)^([ \t]*(?:self|state)\.active\.fetch_sub\(1, Ordering::AcqRel\))$", r"\1;")
sub("src/mux_pool.rs", r"(?m)^([ \t]*)enforce_target_policy\(&cfg, &bind_hint\)\.or_else\(\|_\| Ok\(\(\)\)\)\?;$", r"\1let _ = enforce_target_policy(&cfg, &bind_hint);")
sub("src/mux_pool.rs", r"(?m)^#\[derive\(Debug\)\]\n(?=struct SessionState\b)", "")
sub("src/mux_pool.rs", r"(?m)^([ \t]*)\}\)$\n(?=\s*\}$)", r"\1});")
sub("src/mux_pool.rs", r"(?s)(Arc\.new\(Self \{\n\s*cfg,\n\s*sessions: Mutex::new\(Vec::new\(\)\),\n\s*)\);\n\s*}", r"\1})\n    }")

for path in ("src/nonmux.rs", "src/runtime.rs"):
    sub(path, r"(?m)^([ \t]*)let mut ka = socket2::TcpKeepalive::new\(\);\n\1ka\.with_time\(Duration::from_secs\(30\)\);\n\1let _ = sock\.set_tcp_keepalive\(&ka\);$", r"\1let ka = socket2::TcpKeepalive::new().with_time(Duration::from_secs(30));\n\1let _ = sock.set_tcp_keepalive(&ka);")
    sub(path, r"(?m)^([ \t]*)\}\)$\n(?=\s*\}$)", r"\1});")

sub("src/runtime.rs", r"(?m)^([ \t]*)return ip\.is_loopback\(\) \|\| ip\.is_private\(\) \|\| ip\.is_link_local\(\) \|\| ip\.is_unspecified\(\);$", r"\1return match ip {\n\1    IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified(),\n\1    IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified() || v6.is_unique_local() || v6.is_unicast_link_local(),\n\1};")
sub("src/runtime.rs", r"(?m)^([ \t]*)let addr = resolve_socket\(&target\.host, target\.port\)\.await\?;$", r"\1let addr = resolve_socket(&target.host, target.port).await.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;")

sub("src/quic.rs", r"(?ms)^\s*cfg\.initial_stream_receive_window\(VarInt::from_u64\(2 \* 1024 \* 1024\)\);\s*$\n\s*cfg\.max_stream_receive_window\(VarInt::from_u64\(8 \* 1024 \* 1024\)\);\s*$\n\s*cfg\.initial_connection_receive_window\(VarInt::from_u64\(4 \* 1024 \* 1024\)\);\s*$\n\s*cfg\.max_connection_receive_window\(VarInt::from_u64\(16 \* 1024 \* 1024\)\);", "    cfg.stream_receive_window(VarInt::from_u64(8 * 1024 * 1024).expect(\"8MiB fits QUIC VarInt\"));\n    cfg.receive_window(VarInt::from_u64(16 * 1024 * 1024).expect(\"16MiB fits QUIC VarInt\"));")
sub("src/quic.rs", r"cert\.signing_key\.serialize_der\(\)", "cert.key_pair.serialize_der()")
sub("src/quic.rs", r"(?m)^([ \t]*)\}\)$\n(?=\s*\}$)", r"\1});")
sub("src/quic.rs", r"(?s)(fn new\(\n\s*cfg: QuicConfig,\n\s*\) -> Arc<Self> \{.*?Arc\.new\(Self \{.*?\n\s*)\);\n\s*}", r"\1})\n    }", flags=re.S)

sub("src/wss_client.rs", r"(?m)^#\[derive\(Debug\)\]\n(?=struct WssSessionState\b)", "")
sub("src/wss_client.rs", r"(?m)^#\[derive\(Debug\)\]\n(?=struct WssSessionPool\b)", "")
sub("src/wss_client.rs", r"(?m)^([ \t]*)streams\.insert\(id, tx\)$", r"\1streams.insert(id, tx);")
sub("src/wss_client.rs", r"(?m)^([ \t]*session\.active\.fetch_sub\(1, Ordering::AcqRel\))$", r"\1;")
sub("src/wss_client.rs", r"(?m)^([ \t]*)\}\)$\n(?=\s*\}$)", r"\1});")
sub("src/wss_client.rs", r"(?s)(Arc\.new\(Self \{\n\s*cfg,\n\s*sessions: Mutex::new\(Vec::new\(\)\),\n\s*)\);\n\s*}", r"\1})\n    }")

sub("src/main.rs", r"value_parser = clap::value_parser!\(usize\)\.range\(1\.\.\=64\)", "value_parser = clap::value_parser!(usize)")
sub("src/main.rs", r"value_parser = clap::value_parser!\(usize\)\.range\(1\.\.\=1_000_000\)", "value_parser = clap::value_parser!(usize)")
sub("src/main.rs", r"(?m)^    let args = Args::parse_from\(&raw_args\);\n    let filter =", "    let args = Args::parse_from(&raw_args);\n    if !(1..=64).contains(&args.mux_sessions) {\n        return Err(anyhow!(\"mux-sessions must be between 1 and 64\"));\n    }\n    if let Some(v) = args.max_conn {\n        if !(1..=1_000_000).contains(&v) {\n            return Err(anyhow!(\"max-conn must be between 1 and 1000000\"));\n        }\n    }\n    let filter =")
