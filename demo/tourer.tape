# Records docs/demo.gif, a tour of tourer itself: `vhs demo/tourer.tape` from the repo root.
# Needs vhs (https://github.com/charmbracelet/vhs), a release build, and a tour of this repo
# already planned into target/demo-tour by demo/plan.tape, so the recording does not wait for
# the planner. Each recording starts from a fresh copy of that tour. The question asked below
# is answered live, so each recording differs a little; the cursor moves in the code pane
# may need adjusting so they stop on noted lines. Afterwards, shrink the GIF with
#   gifsicle -O3 --lossy=40 docs/demo.gif -o docs/demo.gif

Output docs/demo.gif
Output docs/demo.mp4

Set Shell "bash"
Set FontSize 15
Set Width 1500
Set Height 860
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 40ms
Set Framerate 12

Hide
Type "export XDG_STATE_HOME=$(mktemp -d) PATH=$PWD/target/release:$PATH PS1='$ ' && mkdir $XDG_STATE_HOME/tourer && cp -R target/demo-tour $XDG_STATE_HOME/tourer/tours && clear"
Enter
Show

Sleep 300ms
Type "tourer --agent claude ."
Sleep 300ms
Enter
Wait+Screen /Continue tour/
Sleep 800ms

# The tour was planned with instructions; show them, then continue it.
Down 2
Sleep 1.5s
Up 2
Enter
Sleep 1.5s

# Step through the first stop: each ↓ moves to the next code reference.
Down@1.4s 2
Sleep 1.4s

# On to the next stop, to its third code reference.
Right
Sleep 1.5s
Down@1s 3
Sleep 1s

# Into the code: the cursor shows each noted line's note, with more detail.
Tab
Sleep 500ms
Down@350ms 6
Sleep 3s

# Ask about the line under the cursor.
Enter
Type "Why not send the request here?"
Sleep 400ms
Enter
Sleep 3s

# Skip the wait for the rest of the answer.
Hide
Wait+Screen@5m /(?m)claude\s*$/
Show
Sleep 1.5s

# Back to the text, through the answer's code references.
Tab
Sleep 400ms
Down@1.5s 3
Sleep 1s

# Open any file, then go back to following the text.
Type "f"
Sleep 500ms
Type "markd"
Sleep 600ms
Enter
Sleep 2s
Escape
Sleep 800ms

# Zoom the text, then the code.
Type "z"
Sleep 1.5s
Tab
Sleep 1.5s
Type "z"
Sleep 500ms

# The list of stops.
Type "o"
Sleep 2.5s
Escape
Sleep 300ms
Type "q"
Sleep 300ms
