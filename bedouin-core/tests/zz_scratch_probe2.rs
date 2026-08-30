use bedouin_core::host::{Cmd, Host, Line, OsHost};

#[test]
fn probe_non_utf8_stdout() {
    let h = OsHost::new();
    let script = "printf 'good1\\n'; printf '\\377\\376 bad\\n'; printf 'good2\\n'; printf 'good3\\n'";
    let mut cmd = Cmd::new(["/bin/sh", "-c", script]);
    cmd.env = std::collections::BTreeMap::new();
    let mut got = Vec::new();
    let st = h.run(&cmd, &mut |l: Line| got.push(format!("{:?}", l))).unwrap();
    println!("exit={} timed_out={}", st.code, st.timed_out);
    for g in &got { println!("LINE {g}"); }
}
