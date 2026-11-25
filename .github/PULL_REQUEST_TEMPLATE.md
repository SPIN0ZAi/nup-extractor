## 📌 Description
<!-- Provide a concise overview of the changes introduced in this Pull Request. -->

## 🔗 Related Issue / Kanban Card
<!-- Link the issue or Kanban card this PR resolves (e.g. Closes #8, Fixes #12) -->
Closes #

## 🛠️ Type of Change
- [ ] 🐛 Bug fix (non-breaking change fixing an issue)
- [ ] ✨ New feature (non-breaking change adding functionality)
- [ ] ⚡ Performance improvement
- [ ] ♻️ Code refactoring / Cleanup
- [ ] 📝 Documentation / Website update

## ✅ Pre-Merge Checklist
- [ ] Formatting & lints pass locally (`cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings`).
- [ ] All automated tests pass locally (`cargo test --workspace` and `dotnet test legacy_dotnet/NupExtractor.slnx -c Release`).
- [ ] Standalone release executable builds cleanly (`cargo build --release -p nup-extractor`).
- [ ] CI Verification workflow passes with green checks.
- [ ] Ready for review and merge.
