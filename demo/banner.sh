#!/usr/bin/env bash
# Intro title card shown for the first ~2s of the demo GIF, before the sidebar
# appears — names the tool and pre-teaches the status glyphs so the rail reads
# instantly when it shows up.
printf '\033[2J\033[3J\033[H'   # clear screen + scrollback so the invoking command line isn't shown
printf '\n\n'
printf '   \033[1;35mzj-radar\033[0m\n\n'
printf '   A \033[1mZellij sidebar\033[0m with live status for every tab —\n'
printf '   your AI agents and long-running commands, at a glance.\n\n\n'
printf '   \033[35m◆\033[0m needs you     \033[36m◐\033[0m working     \033[32m●\033[0m done     \033[31m✗\033[0m error\n'
