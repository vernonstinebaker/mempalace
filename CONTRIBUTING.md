# Contributing to MemPalace

Thank you for considering contributing to MemPalace! This document outlines how to report bugs, suggest features, and submit code changes.

## 🐛 Reporting Bugs

Before reporting a bug, please check if it has already been reported by searching [existing issues](https://orangepi:3000/vds/mempalace/issues).

When reporting a bug, please include:
- **Version**: The MemPalace version you're running (`mempalace-mcp --version` or check Cargo.toml)
- **Environment**: OS, Rust version (`rustc --version`), hardware (Intel/Apple Silicon)
- **Steps to reproduce**: Minimal steps to trigger the issue
- **Expected behavior**: What should happen
- **Actual behavior**: What actually happens (include logs if available)
- **Palace state**: If relevant, output from `mempalace-mcp --info` and `mempalace_mempalace_status`
- **MCP client**: Which assistant you're using (OpenCode, OpenCat, Claude Desktop, etc.)

For crashes or panics, include the full backtrace. You can enable verbose logging by setting `RUST_LOG=debug` before running the binary.

## 💡 Suggesting Features

We welcome feature suggestions! Please open an issue using the "Feature Request" template and include:
- **Clear title**: Concise description of the feature
- **Motivation**: Why this would be useful to users
- **Use case**: Specific scenario where you'd apply this feature
- **Alternative approaches**: Other ways to achieve the same goal (if applicable)
- **Potential drawbacks**: Performance, complexity, or maintenance concerns

Please check if similar functionality can already be achieved with existing tools before suggesting.

## 📥 Making Changes

### Setting Up Your Development Environment

1. **Fork the repository** on orangepi:3000
2. **Clone your fork**:
   ```sh
   git clone https://orangepi:3000/your-username/mempalace.git
   cd mempalace
   ```
3. **Install Rust** if needed (version 1.75+ required):
   ```sh
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   source "$HOME/.cargo/env"
   ```
4. **Build and test**:
   ```sh
   cargo build
   cargo test
   ```

### Code Style

- Run `cargo fmt` before committing to format code according to the project style
- Run `cargo clippy` to catch common mistakes and get suggestions
- Follow the existing code style in the file you're editing
- Keep line lengths reasonable (aim for <100 chars when possible)
- Use descriptive variable and function names
- Add comments for non-obvious logic or complex algorithms

### Making Changes

1. **Create a branch** for your work:
   ```sh
   git checkout -b feature/your-feature-name
   # or
   git checkout -b fix/your-bug-fix
   ```
2. **Make your changes** in small, focused commits
3. **Write clear commit messages** in the format:
   ```
   type(scope): brief description
   
   optional detailed description
   
   Fixes #issue-number
   ```
   Examples:
   - `feat(search): add hybrid retrieval with RRF fusion`
   - `fix(db): handle null bytes in file indexer`
   - `docs(readme): clarify installation steps`
4. **Keep changes focused** – one feature or bug fix per pull request when possible
5. **Update documentation** if your change affects user-facing behavior:
   - README.md
   - Tool descriptions in mcp.rs
   - This CONTRIBUTING.md if needed
   - AGENTS.md if agent guidance changes

### Testing Your Changes

- **Unit tests**: Add or update tests in the relevant file (`#[cfg(test)] mod tests { ... }`)
- **Integration tests**: Run the benchmark suite to check for regressions:
  ```sh
  source /path/to/venv/bin/activate  # if using Python venv for benchmarks
  python bench/longmemeval_rust_useronly.py
  ```
- **Manual testing**: Try your changes with real MCP clients:
  - OpenCode: `~/.config/opencode/config.json`
  - OpenCat: MCP Servers configuration in the app
  - Direct testing: pipe JSON-RPC messages to stdio

### Submitting a Pull Request

1. **Push your branch** to your fork:
   ```sh
   git push origin feature/your-feature-name
   ```
2. **Open a pull request** from your fork's branch to the main repository's `master` branch
3. **Fill out the PR template** completely:
   - Summary of changes
   - Related issue number (if applicable)
   - Checklist items (tests, docs, etc.)
   - Any special notes for reviewers
4. **Respond to feedback** from maintainers promptly
5. **Keep your branch updated** with `master` if needed:
   ```sh
   git fetch origin
   git rebase origin/master
   # resolve any conflicts, then force-push if rebased
   git push -f
   ```

## 📝 License

By contributing to MemPalace, you agree that your contributions will be licensed under the MIT License (see LICENSE file).

## 🙏 Thank You!

Your contributions help make MemPalace a better memory system for everyone. Whether you're fixing a typo, adding a feature, or improving documentation – every contribution matters.

If you have questions during the process, don't hesitate to ask in the issue tracker or reach out to the maintainers.

Happy hacking! 🏰