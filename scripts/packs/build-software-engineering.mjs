#!/usr/bin/env node
// Builds src-tauri/packs/software-engineering.json from the curated list below.
//
//   node scripts/packs/build-software-engineering.mjs
//
// One line per term: `term | spoken forms; … | category | weight | flags`.
// * Spoken forms are what speech recognition writes for the term when it is
//   said aloud. A term split into words ("type script", "git hub") needs none:
//   the retrieval index joins words before matching. List only forms that
//   SOUND different from the spelling ("cube control" for kubectl).
// * Never list a spoken form that is an ordinary phrase on its own ("view"
//   for Vue): it would retrieve the term in every sentence that says it.
// * Flag `ambiguous` marks a term that is also an everyday word (Rust, Swift);
//   it is retrieved only where the transcript capitalises it.
// Categories: product, identifier (commands, crates, file names), abbreviation, term.

import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { phoneticKey as key } from "./phonetic-key.mjs";

const OUTPUT = join(dirname(fileURLToPath(import.meta.url)), "../../src-tauri/packs/software-engineering.json");

const ALWAYS_ON = [
  "kubectl", "Kubernetes", "PostgreSQL", "nginx", "OAuth", "GraphQL", "TypeScript", "JavaScript",
  "JSON", "YAML", "npm", "pnpm", "Tauri", "Vite", "Cargo", "GitHub", "Docker", "Terraform", "Redis",
  "SQLite", "Next.js", "Node.js", "Claude Code", "MCP", "CI/CD", "API", "SSH", "localhost",
  "PR", "README",
];

const TERMS = String.raw`
# Languages
TypeScript | | product | 1
JavaScript | | product | 1
Python | | product | 0.9 | ambiguous
Rust | | product | 0.9 | ambiguous
Go | golang; go lang | product | 0.8 | ambiguous
Java | | product | 0.7 | ambiguous
Kotlin | cotlin | product | 0.7
Swift | | product | 0.7 | ambiguous
SwiftUI | swift u i; swift you eye | product | 0.8
Objective-C | objective c | product | 0.6
C++ | c plus plus; see plus plus | product | 0.7
C# | c sharp; see sharp | product | 0.7
Ruby | | product | 0.6 | ambiguous
PHP | | product | 0.6
Elixir | | product | 0.5 | ambiguous
Erlang | | product | 0.5
Haskell | | product | 0.5
Scala | | product | 0.5
Clojure | | product | 0.5
OCaml | oh camel | product | 0.5
Zig | | product | 0.5 | ambiguous
Dart | | product | 0.5 | ambiguous
Lua | | product | 0.5
Bash | | product | 0.6 | ambiguous
Zsh | z shell; zee shell; zed shell | product | 0.6
PowerShell | | product | 0.6
WebAssembly | web assembly | product | 0.7
Wasm | wasm; was um | abbreviation | 0.6
SQL | | abbreviation | 0.8
Solidity | | product | 0.4 | ambiguous
# Runtimes and package managers
Node.js | | product | 0.9
Deno | dee no | product | 0.7
Bun | | product | 0.6 | ambiguous
npm | | identifier | 1
npx | | identifier | 0.8
pnpm | | identifier | 0.9
Yarn | | identifier | 0.6 | ambiguous
pip | | identifier | 0.6 | ambiguous
pipx | | identifier | 0.5
uv | | identifier | 0.5 | ambiguous
Poetry | | product | 0.4 | ambiguous
conda | | identifier | 0.5
Homebrew | home brew | product | 0.7
Cargo | | identifier | 0.9 | ambiguous
rustup | rust up | identifier | 0.8
rustc | rust c | identifier | 0.7
clippy | | identifier | 0.8 | ambiguous
rustfmt | rust format; rust fmt | identifier | 0.7
crates.io | crates io | product | 0.7
Gradle | | product | 0.5
Maven | | product | 0.5 | ambiguous
CocoaPods | cocoa pods | product | 0.5
JVM | | abbreviation | 0.5
# Frontend
React | | product | 0.9 | ambiguous
React Native | | product | 0.7
Next.js | | product | 0.9
Vue | vue js; vue.js | product | 0.7
Nuxt | nucked; nuxt js | product | 0.6
Svelte | svelt | product | 0.7
SvelteKit | svelte kit; svelt kit | product | 0.6
Angular | | product | 0.6 | ambiguous
SolidJS | solid js | product | 0.5
Astro | | product | 0.5 | ambiguous
Remix | | product | 0.4 | ambiguous
Vite | veet; vit | product | 0.9
Vitest | vee test; veet test; vi test | identifier | 0.8
Webpack | | product | 0.7
esbuild | e s build; ee s build | identifier | 0.6
Rollup | | product | 0.5 | ambiguous
Turbopack | turbo pack | product | 0.5
Turborepo | turbo repo | product | 0.5
Babel | | product | 0.5 | ambiguous
ESLint | e s lint; ee s lint; es lint | identifier | 0.8
Prettier | | identifier | 0.7 | ambiguous
Biome | | product | 0.4 | ambiguous
Tailwind CSS | tail wind css | product | 0.8
PostCSS | post css | product | 0.5
Sass | | product | 0.5 | ambiguous
HTML | | abbreviation | 0.8
CSS | | abbreviation | 0.8
DOM | | abbreviation | 0.6 | ambiguous
JSX | | abbreviation | 0.6
TSX | | abbreviation | 0.6
Storybook | story book | product | 0.5
Playwright | play wright; play right | product | 0.8
Puppeteer | | product | 0.6
Cypress | | product | 0.6 | ambiguous
Jest | | identifier | 0.7 | ambiguous
Mocha | | identifier | 0.4 | ambiguous
Electron | | product | 0.6 | ambiguous
Tauri | tower e; towery; tow ree; tory app | product | 1
Redux | re ducks; reducks | product | 0.6
Zustand | zoo stand; zu stand | product | 0.5
TanStack Query | tan stack query | product | 0.5
shadcn/ui | shad c n; shad cn; shad seen | product | 0.6
Radix UI | radix | product | 0.5
htmx | h t m x | product | 0.5
jQuery | j query; jay query | product | 0.5
Three.js | three js | product | 0.5
D3.js | d three; d3 js | product | 0.5
Figma | | product | 0.7
# Backend and frameworks
Express | express js | product | 0.6 | ambiguous
Fastify | fast ify | product | 0.5
NestJS | nest js | product | 0.5
Django | jango; d jango | product | 0.7
Flask | | product | 0.6 | ambiguous
FastAPI | fast api; fast a p i | product | 0.7
Pydantic | pie dantic; pi dantic | product | 0.6
Rails | ruby on rails | product | 0.5 | ambiguous
Laravel | | product | 0.5
Spring Boot | | product | 0.5
ASP.NET | asp dot net | product | 0.5
.NET | dot net | product | 0.6
Axum | axe um; ax um | identifier | 0.6
Actix | act ix | identifier | 0.5
Tokio | tokio rs | identifier | 0.8
Serde | sir day; ser dee; sur day | identifier | 0.8
reqwest | request crate | identifier | 0.6
anyhow | | identifier | 0.5 | ambiguous
thiserror | this error | identifier | 0.5
clap | | identifier | 0.5 | ambiguous
tracing | | identifier | 0.4 | ambiguous
rayon | | identifier | 0.5 | ambiguous
hyper | | identifier | 0.4 | ambiguous
tonic | | identifier | 0.4 | ambiguous
wgpu | w g p u; web gpu | identifier | 0.5
bindgen | bind gen | identifier | 0.5
whisper.cpp | whisper cpp; whisper c p p | product | 0.7
llama.cpp | llama cpp; llama c p p; lama cpp | product | 0.8
ONNX | on x | product | 0.7
ONNX Runtime | | product | 0.6
PyTorch | pie torch; pi torch | product | 0.7
TensorFlow | tensor flow | product | 0.6
NumPy | num pie; numb pie; num pi | product | 0.7
pandas | | identifier | 0.5 | ambiguous
Jupyter | jupyter notebook | product | 0.7
Hugging Face | | product | 0.8
LangChain | lang chain | product | 0.5
Ollama | o llama; oh llama | product | 0.7
MLX | m l x | product | 0.6
CUDA | cooda; coo da | product | 0.7
Metal | | product | 0.4 | ambiguous
gRPC | g r p c; gee rpc | abbreviation | 0.7
Protobuf | proto buf; proto buff | product | 0.6
tRPC | t r p c | product | 0.5
WebSocket | web socket | term | 0.7
WebRTC | web r t c | abbreviation | 0.5
# Databases and data
PostgreSQL | postgres; post gres; post gress; postgres q l; post gress q l | product | 1
MySQL | my sequel; my s q l | product | 0.7
SQLite | sequel lite; sequel light; s q lite; sql light | product | 0.9
MongoDB | mongo db; mongo | product | 0.7
Redis | reddis; red is | product | 0.9
Valkey | val key | product | 0.4
Elasticsearch | elastic search | product | 0.6
OpenSearch | open search | product | 0.4
Cassandra | | product | 0.4
DynamoDB | dynamo db; dynamo | product | 0.6
ClickHouse | click house | product | 0.5
DuckDB | duck db | product | 0.6
Snowflake | | product | 0.5 | ambiguous
BigQuery | big query | product | 0.6
Kafka | | product | 0.6
RabbitMQ | rabbit m q; rabbit mq | product | 0.5
Prisma | | product | 0.6
Drizzle | | product | 0.4 | ambiguous
Supabase | supa base; super base | product | 0.8
Firebase | fire base | product | 0.7
Neon | | product | 0.3 | ambiguous
PlanetScale | planet scale | product | 0.4
pgvector | p g vector; pg vector | identifier | 0.5
ORM | | abbreviation | 0.6
# Cloud, infrastructure and DevOps
AWS | | abbreviation | 0.9
Amazon S3 | s3; s three | product | 0.7
EC2 | e c two; ec two | product | 0.6
Lambda | | product | 0.6 | ambiguous
CloudFront | cloud front | product | 0.5
IAM | i a m | abbreviation | 0.6
Azure | | product | 0.7 | ambiguous
GCP | | abbreviation | 0.6
Google Cloud | | product | 0.6
Cloud Run | | product | 0.5
Cloudflare | cloud flare | product | 0.8
Cloudflare Workers | cloud flare workers | product | 0.6
Vercel | ver cell; vursel; versel | product | 0.8
Netlify | net lify; net li fy | product | 0.6
Fly.io | fly io; fly dot io | product | 0.5
Render | | product | 0.3 | ambiguous
Railway | | product | 0.4 | ambiguous
Heroku | hero ku; her oku | product | 0.6
DigitalOcean | digital ocean | product | 0.5
Hetzner | hets ner; het sner | product | 0.5
Docker | | product | 1
Docker Compose | docker compose | identifier | 0.8
Dockerfile | docker file | identifier | 0.8
Podman | pod man | product | 0.5
Kubernetes | cooper netties; kuber netties; kuber nettis; cube er netties | product | 1
k8s | k eights; k eight s | abbreviation | 0.6
kubectl | cube control; kube control; kube cuttle; cube cuddle; cube cuttle; kube c t l; cube c t l; kube ctl; cube ctl | identifier | 1
Helm | | product | 0.6 | ambiguous
Terraform | terra form | product | 0.9
OpenTofu | open tofu | product | 0.4
Pulumi | pull umi; pu lumi | product | 0.5
Ansible | | product | 0.6
nginx | engine x; engine ex; n jinx | identifier | 1
Caddy | | product | 0.4 | ambiguous
Traefik | traffic proxy | product | 0.4
HAProxy | h a proxy | product | 0.4
Prometheus | | product | 0.6
Grafana | gra fana; graf ana | product | 0.7
Datadog | data dog | product | 0.6
Sentry | | product | 0.5 | ambiguous
OpenTelemetry | open telemetry | product | 0.6
PagerDuty | pager duty | product | 0.5
Tailscale | tail scale | product | 0.8
WireGuard | wire guard | product | 0.5
systemd | system d | identifier | 0.6
cron | | identifier | 0.5 | ambiguous
crontab | cron tab | identifier | 0.5
launchd | launch d | identifier | 0.4
VPC | | abbreviation | 0.5
CDN | | abbreviation | 0.6
DNS | | abbreviation | 0.7
TLS | | abbreviation | 0.6
SSL | | abbreviation | 0.6
HTTPS | | abbreviation | 0.6
HTTP | | abbreviation | 0.7
SSH | | abbreviation | 0.9
VPN | | abbreviation | 0.6
CI/CD | c i c d; ci cd | abbreviation | 0.9
CI | | abbreviation | 0.8
DevOps | dev ops | term | 0.6
SRE | | abbreviation | 0.5
# Version control and collaboration
Git | | identifier | 0.9 | ambiguous
GitHub | | product | 1
GitHub Actions | | product | 0.8
GitLab | git lab | product | 0.7
Bitbucket | bit bucket | product | 0.5
gh | | identifier | 0.5
PR | | abbreviation | 0.9
pull request | | term | 0.8
rebase | re base | term | 0.8
cherry-pick | cherry pick | term | 0.7
force-push | force push | term | 0.6
worktree | work tree | term | 0.7
monorepo | mono repo | term | 0.7
lockfile | lock file | term | 0.6
.gitignore | git ignore; dot git ignore | identifier | 0.7
README | read me; readme | identifier | 0.9
CHANGELOG | change log | identifier | 0.6
LGTM | | abbreviation | 0.6
WIP | | abbreviation | 0.5
Jira | jeera; gira | product | 0.7
Linear | | product | 0.5 | ambiguous
Confluence | | product | 0.5 | ambiguous
Notion | | product | 0.5 | ambiguous
Slack | | product | 0.6 | ambiguous
Discord | | product | 0.5 | ambiguous
# Editors, terminals and AI tools
VS Code | v s code; vs code; visual studio code | product | 0.8
Vim | | identifier | 0.6
Neovim | neo vim | identifier | 0.6
Emacs | e max; ee max | identifier | 0.5
JetBrains | jet brains | product | 0.5
IntelliJ | intelli j; intellij | product | 0.5
Xcode | x code; ex code | product | 0.8
Android Studio | | product | 0.5
iTerm2 | i term; eye term; iterm two | product | 0.5
tmux | t mux; tee mux | identifier | 0.6
Ghostty | ghosty; ghost tea | product | 0.4
Warp | | product | 0.3 | ambiguous
Claude | clawed; clod | product | 0.9
Claude Code | clawed code; clod code | product | 0.9
Anthropic | an thropic | product | 0.7
OpenAI | open a i; open ai | product | 0.8
ChatGPT | chat g p t; chat gpt | product | 0.8
Codex | | product | 0.6 | ambiguous
GitHub Copilot | git hub co pilot | product | 0.7
Copilot | co pilot | product | 0.6
Cursor | | product | 0.6 | ambiguous
Windsurf | wind surf | product | 0.4
Gemini | | product | 0.5 | ambiguous
LLM | | abbreviation | 0.8
LLMs | | abbreviation | 0.6
MCP | | abbreviation | 0.9
MCP server | m c p server | term | 0.7
RAG | | abbreviation | 0.6 | ambiguous
LoRA | | abbreviation | 0.6
GGUF | g g u f; gguf | abbreviation | 0.6
embeddings | | term | 0.5 | ambiguous
prompt caching | | term | 0.4
Parakeet | | product | 0.5 | ambiguous
Whisper | | product | 0.6 | ambiguous
# Command line
sudo | sue do; sue dough | identifier | 0.8
chmod | ch mod; c h mod; chamod | identifier | 0.7
chown | ch own; c h own | identifier | 0.6
grep | g rep | identifier | 0.8
ripgrep | rip grep | identifier | 0.6
rg | | identifier | 0.3
sed | | identifier | 0.5 | ambiguous
awk | | identifier | 0.5 | ambiguous
jq | j q; jay q | identifier | 0.6
curl | | identifier | 0.7 | ambiguous
wget | w get | identifier | 0.5
ssh-keygen | ssh keygen; ssh key gen | identifier | 0.5
rsync | r sync; are sync | identifier | 0.6
scp | | identifier | 0.4
tar | | identifier | 0.4 | ambiguous
gzip | g zip; gee zip | identifier | 0.4
xargs | x args | identifier | 0.4
stdout | standard out; std out | term | 0.7
stderr | standard error; std err | term | 0.7
stdin | standard in; std in | term | 0.6
/dev/null | dev null | identifier | 0.6
PATH | | identifier | 0.3 | ambiguous
env var | n var; envy var | term | 0.6
.env | dot env | identifier | 0.7
dotfiles | dot files | term | 0.5
localhost | local host | identifier | 0.9
127.0.0.1 | one two seven dot zero dot zero dot one | identifier | 0.4
macOS | mac o s; mac os | product | 0.8
Linux | | product | 0.8
Ubuntu | | product | 0.6
Debian | | product | 0.5
Alpine | | product | 0.4 | ambiguous
WSL | | abbreviation | 0.5
# File formats and data formats
JSON | | abbreviation | 1
JSONL | json l | abbreviation | 0.5
YAML | yammel; yamel; yam l | abbreviation | 1
TOML | tommel; tom l | abbreviation | 0.8
XML | | abbreviation | 0.6
CSV | | abbreviation | 0.7
Markdown | mark down | term | 0.8
package.json | package json; package dot json | identifier | 0.8
package-lock.json | package lock json | identifier | 0.5
tsconfig.json | ts config; t s config | identifier | 0.7
Cargo.toml | cargo toml; cargo tommel | identifier | 0.8
Cargo.lock | cargo lock | identifier | 0.6
lib.rs | lib dot r s; lib rs | identifier | 0.6
main.rs | main dot r s; main rs | identifier | 0.6
vite.config.ts | vite config | identifier | 0.5
docker-compose.yml | docker compose yaml; docker compose yml | identifier | 0.5
Makefile | make file | identifier | 0.6
.zshrc | z s h r c; zshrc | identifier | 0.4
UTF-8 | u t f eight; utf eight | abbreviation | 0.6
Base64 | base sixty four; base 64 | term | 0.6
regex | redge ex; reg ex | term | 0.8
glob | | term | 0.4 | ambiguous
SVG | | abbreviation | 0.6
PNG | ping file | abbreviation | 0.5
WebP | web p | abbreviation | 0.4
PDF | | abbreviation | 0.6
WAV | | abbreviation | 0.4 | ambiguous
# Web and API concepts
API | | abbreviation | 1
APIs | | abbreviation | 0.7
REST | | abbreviation | 0.6 | ambiguous
GraphQL | graph q l; graph ql; graph cue l | abbreviation | 1
OAuth | oh auth; o auth | abbreviation | 1
OIDC | o i d c | abbreviation | 0.5
JWT | j w t | abbreviation | 0.8
SSO | | abbreviation | 0.6
SAML | sam l; sammel | abbreviation | 0.4
CORS | course headers; cors | abbreviation | 0.7
CSP | | abbreviation | 0.5
CSRF | c surf; sea surf | abbreviation | 0.5
XSS | | abbreviation | 0.5
CRUD | | abbreviation | 0.5 | ambiguous
SDK | | abbreviation | 0.8
CLI | | abbreviation | 0.9
GUI | | abbreviation | 0.5
UI | | abbreviation | 0.8
UX | | abbreviation | 0.6
URL | | abbreviation | 0.8
URI | | abbreviation | 0.4
UUID | u u i d | abbreviation | 0.6
IPC | | abbreviation | 0.6
RPC | | abbreviation | 0.5
CDP | | abbreviation | 0.3
SPA | | abbreviation | 0.4 | ambiguous
SSR | | abbreviation | 0.5
SEO | | abbreviation | 0.4
webhook | web hook | term | 0.7
middleware | middle ware | term | 0.6
endpoint | end point | term | 0.6
async | a sync; ay sync | term | 0.8
await | | term | 0.5 | ambiguous
Promise | | term | 0.3 | ambiguous
callback | call back | term | 0.4 | ambiguous
closure | | term | 0.3 | ambiguous
mutex | mew tex; mute ex | term | 0.7
semaphore | sema four | term | 0.5
deadlock | dead lock | term | 0.5
race condition | | term | 0.5
idempotent | i dem potent; idem potent | term | 0.6
serialization | | term | 0.4
deserialize | de serialize | term | 0.5
refactor | re factor | term | 0.6
linter | | term | 0.5
transpile | trans pile | term | 0.4
polyfill | poly fill | term | 0.4
tree-shaking | tree shaking | term | 0.4
hydration | | term | 0.3 | ambiguous
Wi-Fi | wifi; why fi | term | 0.3
# Testing, quality and debugging
unit test | | term | 0.5
end-to-end test | end to end test; e2e test | term | 0.5
E2E | e two e; e to e | abbreviation | 0.5
flaky test | flakey test | term | 0.5
snapshot test | | term | 0.4
stack trace | | term | 0.6
segfault | seg fault | term | 0.6
OOM | o o m; out of memory | abbreviation | 0.5
repro | re pro | term | 0.5
nit | | term | 0.4 | ambiguous
TODO | to do | identifier | 0.4 | ambiguous
TDD | | abbreviation | 0.4
QA | | abbreviation | 0.5
# Concepts and acronyms
CPU | | abbreviation | 0.7
GPU | | abbreviation | 0.8
NPU | | abbreviation | 0.5
RAM | | abbreviation | 0.6 | ambiguous
SSD | | abbreviation | 0.5
OS | | abbreviation | 0.4
ARM64 | arm sixty four; arm 64 | abbreviation | 0.5
x86 | x eighty six; x 86 | abbreviation | 0.5
Apple Silicon | | term | 0.6
M1 | m one | identifier | 0.4
LTS | | abbreviation | 0.5
SemVer | sem ver; semver | term | 0.6
MIT License | m i t license | term | 0.5
Apache-2.0 | apache two; apache 2.0 | term | 0.4
CRDT | c r d t | abbreviation | 0.4
FFI | | abbreviation | 0.5
ABI | | abbreviation | 0.4
AST | | abbreviation | 0.4
JIT | | abbreviation | 0.4
GC | | abbreviation | 0.4
WASI | wazzy; wah see | abbreviation | 0.4
P99 | p ninety nine | abbreviation | 0.4
SLA | | abbreviation | 0.4
KPI | | abbreviation | 0.3
MVP | | abbreviation | 0.5
POC | | abbreviation | 0.4
`;

function parse(text) {
  const terms = [];
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    const [term, spoken = "", category = "term", weight = "0.5", flags = "", accept = ""] = trimmed
      .split("|")
      .map((part) => part.trim());
    const spokenForms = spoken
      .split(";")
      .map((s) => s.trim())
      .filter((s) => s && s.toLowerCase() !== term.toLowerCase());
    const entry = { term, category, weight: Number(weight) };
    if (spokenForms.length) entry.spoken_forms = [...new Set(spokenForms)];
    if (accept) entry.accept = accept.split(";").map((s) => s.trim()).filter(Boolean);
    if (flags.split(/\s+/).includes("ambiguous")) entry.ambiguous = true;
    if (!Number.isFinite(entry.weight)) throw new Error(`Bad weight on ${term}`);
    terms.push(entry);
  }
  return terms;
}

// Duplicates are checked by the app's own normalisation (phonetic-key.mjs,
// ported from phonetic_index.rs), so C, C++ and C# stay apart here exactly
// as they do in the app.
const terms = parse(TERMS);
const seen = new Map();
for (const term of terms) {
  if (seen.has(key(term.term))) throw new Error(`Duplicate term: ${term.term} / ${seen.get(key(term.term))}`);
  seen.set(key(term.term), term.term);
}
const missing = ALWAYS_ON.filter((t) => !seen.has(key(t)));
if (missing.length) throw new Error(`always_on terms missing: ${missing.join(", ")}`);

const pack = {
  schema: 1,
  id: "software-engineering",
  name: "Software engineering",
  description:
    "Languages, frameworks, tools, cloud services, command-line programs, file formats and acronyms, with the ways speech recognition tends to mishear them.",
  version: "2026.10.1",
  locale: "en",
  licence: "MIT",
  attribution: "Curated by the Fairspoken contributors. Product names are trademarks of their owners and are used only to spell them.",
  sources: [
    {
      name: "Fairspoken curated list",
      url: "",
      licence: "MIT",
      used_for: "Every term and spoken form (scripts/packs/build-software-engineering.mjs).",
    },
  ],
  always_on: ALWAYS_ON,
  terms,
  corrections: [],
  snippets: [],
};
writeFileSync(OUTPUT, `${JSON.stringify(pack, null, 1)}\n`);
console.error(`Wrote ${OUTPUT}: ${terms.length} terms.`);
