# Bundled fonts

Vendored from the system `fonts-dejavu-core` package (DejaVu Fonts license,
see `LICENSE` in this directory — permissive, free for redistribution and
embedding in binaries). Bundling these lets `morph-render` produce
deterministic output on any machine, including minimal containers with no
system fonts installed, without depending on fontconfig at runtime.

- `DejaVuSans.ttf` / `-Bold` / `-Oblique` — UI/document text (Markdown, JSON, tables).
- `DejaVuSansMono.ttf` / `-Bold` — code and terminal log rendering.
