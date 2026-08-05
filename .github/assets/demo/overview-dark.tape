# Overview: a real Herdr client opens the Port Forward dashboard overlay. Run with: make demo
Set Shell "bash"
Set FontFamily "FiraCode Nerd Font"
Set FontSize 17
Set Width 1920
Set Height 1080
Set Padding 18
Set TypingSpeed 35ms
Output .github/assets/demo/overview-dark.gif
Set Theme "Dracula+"

Env HERDR_FWD_DEMO_ROOT "/private/tmp/herdr-fwd-vhs-overview"
Env HERDR_FWD_DEMO_THEME "catppuccin"
Env COLORFGBG "15;0"

Hide
Type "bash .github/assets/demo/overview.sh" Enter
Sleep 30s
Show
Sleep 10s
Type "q"
Sleep 1s
Ctrl+C
