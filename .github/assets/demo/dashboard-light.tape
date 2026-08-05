# Dashboard: select Vite and pause its forward. Run with: make demo
Set Shell "bash"
Set FontFamily "FiraCode Nerd Font"
Set FontSize 17
Set Width 1440
Set Height 560
Set Padding 18
Set TypingSpeed 35ms
Output .github/assets/demo/dashboard-light.gif
Set Theme "Github"

Env HERDR_FWD_DEMO_ROOT "/private/tmp/herdr-fwd-vhs-dashboard"
Env COLORFGBG "0;15"

Hide
Type "source .github/assets/demo/setup.sh" Enter
Sleep 1s
Type "set +e; $HERDR_FWD_DEMO_BIN dashboard $HERDR_FWD_DEMO_SESSION; herdr_fwd_demo_cleanup" Enter
Sleep 1s
Show
Sleep 2s
Type "j"
Sleep 1s
Type " "
Sleep 2s
Hide
Type "q"
Sleep 1s
