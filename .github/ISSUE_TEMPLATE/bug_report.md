---
name: Bug Report
about: Report a bug or unexpected behavior in IRIS
title: "[Bug] "
labels: bug
assignees: ''
---

## Description

A clear and concise description of the bug.

## Steps to Reproduce

1. Create a file `example.iris` with the following content:
```iris
def main() -> i64 {
    // minimal reproduction code
    return 0
}
```
2. Run: `iris run example.iris`
3. See error

## Expected Behavior

What you expected to happen.

## Actual Behavior

What actually happened. Include the full error message or output.

## Environment

- **IRIS version**: (run `iris --version`)
- **OS**: (e.g., Windows 11, Ubuntu 24.04, macOS 15)
- **Rust version** (if building from source): (run `rustc --version`)

## Additional Context

Add any other context about the problem here (screenshots, related issues, etc.).
