# Records docs/demo.gif, a tour of tourer itself: `vhs demo/tourer.tape` from the repo root.
# Needs vhs (https://github.com/charmbracelet/vhs), a release build, and a tour of this repo
# already planned into target/demo-tour, so the recording does not wait for the planner:
#   XDG_STATE_HOME=target/demo-plan target/release/tourer --agent claude .
#   mkdir -p target/demo-tour && cp target/demo-plan/tourer/tours/*.json target/demo-tour/
# Each recording starts from a fresh copy of that tour. The question asked below is
# answered live, so each recording differs a little. Afterwards, shrink the GIF with
#   gifsicle -O3 --lossy=40 docs/demo.gif -o docs/demo.gif

Output docs/demo.gif
Output docs/demo.mp4

Set Shell "bash"
Set FontSize 15
Set Width 1500
Set Height 860
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 55ms
Set Framerate 12

Hide
Type "export XDG_STATE_HOME=$(mktemp -d) PATH=$PWD/target/release:$PATH PS1='$ ' && mkdir $XDG_STATE_HOME/tourer && cp -R target/demo-tour $XDG_STATE_HOME/tourer/tours && clear"
Enter
Show

Sleep 500ms
Type "tourer --agent claude ."
Sleep 400ms
Enter
Wait+Screen /Continue tour/
Sleep 1.5s

# Continue the saved tour.
Enter
Sleep 2.5s

# Step through the first stop: each ↓ moves to the next code reference.
Down
Sleep 1.8s
Down
Sleep 1.8s
Down
Sleep 1.8s

# On to the next stops.
Right
Sleep 2.5s
Down
Sleep 1.8s
Right
Sleep 2.5s

# Ask about this stop.
Down@600ms 6
Sleep 1s
Type "?"
Sleep 500ms
Type "Where does a question go between the TUI and the agent?"
Sleep 600ms
Enter
Sleep 5s

# Skip the wait for the rest of the answer.
Hide
Wait+Screen@5m /(?m)claude\s*$/
Show
Sleep 2s
Down
Sleep 2s
Down
Sleep 2s
Down
Sleep 3s

# The list of stops.
Type "o"
Sleep 3s
Escape
Sleep 1s
Type "q"
Sleep 500ms
