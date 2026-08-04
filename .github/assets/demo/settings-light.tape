# Settings: open the integration menu from the dashboard. Run with: make demo
Set Shell "bash"
Set FontFamily "FiraCode Nerd Font"
Set FontSize 17
Set Width 1440
Set Height 560
Set Padding 18
Set TypingSpeed 35ms
Output .github/assets/demo/settings-light.gif
Set Theme "Github"

Env HERDR_FWD_DEMO_ROOT "/private/tmp/herdr-fwd-vhs-settings"
Env COLORFGBG "0;15"

Hide
Type "source .github/assets/demo/setup.sh" Enter
Sleep 1s
Type "$HERDR_FWD_DEMO_BIN dashboard $HERDR_FWD_DEMO_SESSION; herdr_fwd_demo_cleanup" Enter
Sleep 1s
Type "h"
Sleep 1s
Show
Sleep 4s
Type "Esc"
Type "q"
