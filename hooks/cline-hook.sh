#!/bin/bash
# Cline hook — place this file (or symlink it) at:
#   ~/Documents/Cline/Rules/Hooks/TaskCancel
#
# Cline fires this hook when the user cancels a task or the task
# errors out. (TaskComplete is not supported yet.)
zestful notify --agent "cline" --message "Task cancelled — needs attention" --severity warning --app "Cursor"
