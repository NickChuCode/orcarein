# OrcaRein landing page

Static landing page served at **https://orcarein.net/**
(also reachable at https://nickchucode.github.io/orcarein/).

Self-contained: `index.html` (verbatim CSS from the design), `app.js` (pixel-orca
canvas, snowfall, terminal demo, vim keybindings, language toggle, help overlay),
and `assets/` (logos). No build step, no framework, no runtime dependencies —
fonts load from Google Fonts + jsdelivr CDNs.

## Source of truth

The design lives in Claude Design ("Orcarein 网站设计方案"). This directory is a
faithful hand-portable reproduction of `OrcaRein Landing.dc.html`: the Claude
Design component (`<x-dc>` template + `DCLogic` class) was compiled to a plain
static site so it can be hosted anywhere without the Claude Design runtime.

## Deploy

`.github/workflows/pages.yml` deploys this folder to GitHub Pages via GitHub
Actions on any push that touches `site/`. One-time: enable **Settings → Pages →
Source = GitHub Actions**. The `CNAME` file pins the custom domain `orcarein.net`
so each deploy keeps it (DNS is managed at Cloudflare, records point at the
GitHub Pages IPs, DNS-only / grey-cloud).

## Local preview

```sh
cd site && python -m http.server 8799   # then open http://localhost:8799/
```
