# Plans the tour that demo/tourer.tape records: `vhs demo/plan.tape` from the repo root, after
# `cargo build --release`. It starts the plan and quits; the daemon keeps planning in the
# background and saves the tour to target/demo-plan/tourer/tours/ when it is done (a few
# minutes). Then copy it where the demo looks for it:
#   mkdir -p target/demo-tour && cp target/demo-plan/tourer/tours/*.json target/demo-tour/

Output target/demo-plan.gif

Set Shell "bash"
Set Width 1400
Set Height 800

Type "rm -rf target/demo-plan && XDG_STATE_HOME=$PWD/target/demo-plan target/release/tourer --agent claude ."
Enter
Wait+Screen@60s /Start tour/
Down
Type "Start at main.rs and follow one question from the TUI to the agent and back."
Sleep 500ms
Enter
Sleep 10s
Type "q"
Sleep 1s
