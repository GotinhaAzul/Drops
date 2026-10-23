# Pi reference

The Rust engine is a port of the small streaming → tool execution → continuation pattern in [earendil-works/pi](https://github.com/earendil-works/pi), inspected at commit `3953e9ef497024d13c6f4aa646d0adda2e37b0f8` on 2026-10-23. The inspected TypeScript agent loop and its MIT license are retained in `references/`. Pi's runtime is TypeScript; directly embedding it would conflict with the settled Rust architecture, so Drops adapts its ideas and preserves attribution for translated code.

The user-facing product is Drops. Cargo crate names and binaries retain `buddy-*`, `buddyd`, and `buddyctl` as specified in the technical decisions.
