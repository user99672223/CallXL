#!/usr/bin/env bash
# Run a cargo command and print only compiler errors (C++ and Rust) compactly.
"$@" 2>&1 | grep -E "error(\[E[0-9]+\])?:|^\s+--> |warning: unused|^warning: [a-z]" | grep -v "ToolExecError\|libva not found\|cuda.h not found" | sed 's/^warning: webrtc-sys@0.3.53: //' | head -60
echo "exit=${PIPESTATUS[0]}"
