# Polish input contract

How one dictation is presented to a polish model. Every prompt variant that
takes tags builds its user turn with `polish_input::user_turn`
(`src-tauri/src/polish_input.rs`); change that function and this page
together.

## The user turn

The optional tags come first, in this order and each on its own line. Then
the transcript, a blank line and the anchor line:

```text
<spelling>term, term</spelling>
<format>email|chat|document|notes|code|plain</format>
<tone>casual|neutral|formal</tone>
<transcript>…</transcript>

Output only the cleaned transcript.
```

A tag is omitted when it carries no information:

| Tag | Omitted when | Source |
| --- | --- | --- |
| `<spelling>` | no dictionary term sounds like anything in the transcript | `transcript_cleanup::relevant_vocabulary` over the user's dictionary and the session's screen terms |
| `<format>` | the format is `plain` | the format decision made at recording start (below) |
| `<tone>` | the tone is `neutral` | the `polish_tones` setting row for the destination (below) |

A dictation with no tags is byte-identical to the user turn before format
and tone existed.

Examples:

```text
<transcript>so I think we should go with the second option</transcript>

Output only the cleaned transcript.
```

```text
<spelling>Railway</spelling>
<format>email</format>
<tone>formal</tone>
<transcript>hi mary we moved the deploy to rail way thanks sam</transcript>

Output only the cleaned transcript.
```

## What stays fixed

The system prompt and the worked examples never vary per dictation, so
llama-server's prompt cache covers them and each pass pays only for its own
tags and transcript. Tone used to be appended to the system prompt; it is the
`<tone>` tag now. The examples include `<format>` and `<tone>` cases (an email
with a spoken greeting, paragraph break and sign-off; an email where none was
spoken; a casual chat line; a notes list). They come before the plain
examples: replayed last, they taught Qwen3.5 0.8B to copy its input, and its
plain bench score fell from 17/23 to 13/23.

Never put window titles, URLs or text from around the caret into a local
prompt. Small models repeat any lead-in they are shown. The destination
reaches the model only as the compact `<format>` label.

`validate_output` rejects any output containing `<transcript`, `<spelling`,
`<format` or `<tone` (`polish_input::ECHO_MARKERS`).

## Values

### `<format>`

Formatting restructures the spoken words only. It never adds words.

| Value | Layout |
| --- | --- |
| `email` | A spoken greeting ("hi Mary") goes alone on the first line, ending with a comma. Paragraphs break at topic shifts and where "new paragraph" is spoken. A spoken sign-off ("thanks", "best", "cheers" and the name after it) goes on its own lines. A greeting, sign-off or name that was not spoken is never added. |
| `chat` | Short, one block, no added structure. With a casual tone a single short sentence may drop its final period. |
| `document` | Full sentences in paragraphs. |
| `notes` | An enumeration may become a `- ` bullet list. |
| `code` | Identifiers, symbols, file names and casing exactly as spoken, no prose rewriting. Terminals never get polish at all. |
| `plain` | The cleanup with no layout opinion: the behaviour before formats. |

### `<tone>`

`casual`, `neutral` or `formal`, from the `polish_tones` setting (`default` is
`neutral`; `off` skips polish and never reaches a prompt). The row is the
destination app's category. For a browser or an unrecognised app (category
`other`) it is the row matching the decided format: email → Email, chat →
Messaging, document and notes → Documents, code → Code editors, plain →
Everything else. A Gmail tab therefore uses the Email tone.

## Which variants read the tags

| Variant | Gets |
| --- | --- |
| `PolishPrompt::Instructed` (Qwen3.5 0.8B/2B/4B) | the system prompt, all examples, and the tagged user turn |
| `PolishPrompt::Trained` (SpeakoFlow Mini) | its own training prompt and the bare transcript, no tags |
| Fairspoken Cloud (`/v1/polish`) | JSON, not this text layout: `vocabulary` for `<spelling>`, `appContext.tone` for `<tone>`, and an optional top-level `format` (omitted when `plain`) for `<format>`. A Worker that predates `format` ignores it. |

A model retrained on these tags should get a new `PolishPrompt` variant that
calls `polish_input::user_turn`, so the layout above stays the only one.

## Deciding the format

`format_context.rs` decides at recording start, on a background thread, so
recording start never waits for it. On macOS it reads the focused app once
through Accessibility (`macos_ax::focus_context`, bounded by
`with_ax_timeout`): the window title; the focused field's role, subrole, role
description, placeholder, description and help (never its value); and for
Safari, Chrome, Arc, Edge, Brave and Firefox (and other Chromium browsers) the
page URL. The URL comes from the `AXWebArea` holding the focused element
(`AXURL`), else the window's `AXDocument`, else the first web area in a small
search of the window, and is cut to scheme, host and path: no query string,
fragment, credentials or port. None of this is persisted or shown to a polish
model. Password managers are skipped entirely. Other platforms read nothing,
and every dictation there is `plain`.

Order, first match wins:

1. **User mapping** for the site (`site:<host>`) or app (`app:<bundle id>`),
   set in Settings → Text polish → Formatting. Source `user`.
2. **Rules.** Source `rule`.
3. **A label the classifier learned earlier** for the site or app. Source `llm`.
4. **Nothing applied**: `plain`, source `default`. The classifier may run (below).

Then, whatever decided it: a single-line field (`AXTextField`, `AXComboBox`,
`AXSearchField`, such as an email subject or a search box) turns `email`,
`document` and `notes` into `plain`, because they lay out with line breaks.

### Rules

| Destination | Format |
| --- | --- |
| mail.google.com (except `/chat`), outlook.office.com, outlook.office365.com, outlook.live.com, outlook.cloud.microsoft, mail.yahoo.com, app.fastmail.com, mail.proton.me, app.hey.com, mail.zoho.com, mail.superhuman.com | email |
| Apple Mail, Outlook, Superhuman, Spark, Mimestream, Airmail, MailMate, Thunderbird | email |
| A window title starting `Re:`, `Fwd:`, `Fw:`, `AW:`, `SV:`, `Antw:`, `WG:`, `TR:` (any app) | email, reply |
| Gmail/Outlook in a tab title (` - Gmail`, ` - Outlook`), a field labelled "message body" | email |
| docs.google.com/document, notion.so, notion.site, coda.io, quip.com, paper.dropbox.com | document |
| Word, Pages, Notion, Scrivener, TextEdit, Ulysses, iA Writer; ` - Google Docs`, ` - Notion` in a title | document |
| Apple Notes, Obsidian, Bear, Logseq, Drafts, Evernote, OneNote; keep.google.com, evernote.com, workflowy.com, roamresearch.com | notes |
| slack.com, teams.microsoft.com, teams.live.com, teams.cloud.microsoft, discord.com, web.whatsapp.com, web.telegram.org, messenger.com, chat.google.com, mail.google.com/chat, messages.google.com, app.element.io, linkedin.com/messaging, x.com/messages, instagram.com/direct | chat |
| Slack, Messages, WhatsApp, Discord, Telegram, Teams, Zoom, Signal, Messenger; Slack/Teams/Discord/WhatsApp in a title; a field labelled "type a message" or "message #…" | chat |
| github.com, gist.github.com, github.dev, gitlab.com, bitbucket.org, vscode.dev, replit.com, codesandbox.io, stackoverflow.com; ` · GitHub` in a title | code |
| VS Code, Cursor, JetBrains IDEs, Zed, Sublime Text, Xcode (and terminals, which skip polish anyway) | code |
| claude.ai, chatgpt.com, chat.openai.com, gemini.google.com, perplexity.ai, copilot.microsoft.com, poe.com, chat.mistral.ai, chat.deepseek.com, grok.com, google.com, bing.com, duckduckgo.com; the Claude and ChatGPT apps | plain |
| docs.google.com other than documents (Sheets, Slides, Forms) | plain |
| Anything else | plain (classifier candidate) |

Site rules match the host and its subdomains (`acme.slack.com`), except the
search engines, which match exactly. Obsidian and Apple Notes are `notes`
rather than `document` because short entries and lists are their norm; Notion
is a page and wiki tool, so it is `document`. A user mapping changes either.

A decision is for the app it was made in. If the dictation is pasted into a
different app (the user switched mid-dictation), it is `plain`.

### The classifier

Off by default: Settings → Text polish → Formatting → "Detect format with AI
for unknown apps". It runs only when nothing above applied and there is a key
to remember the answer under (a site host, or a native app's bundle id; a
browser whose URL could not be read has none), and only once the recording has
lasted 2 s, so a short dictation never pays for it. It gets 1.5 s.

- **Input:** app name, window title, site host and field labels. **Output:** one
  label; anything else is treated as unavailable.
- **Provider:** whichever polish uses. Locally it runs on the already loaded
  llama-server, never loads a model for itself, and gives up rather than wait
  out a polish pass (`LocalModels::complete_short`). The runtime keeps
  `--parallel 1`: streaming polish waits for the classifier before its first
  pass (`format_context::FIRST_PASS_WAIT`), so the two never queue behind
  each other mid-dictation, and every pass of the dictation uses one format.
  The classifier's prompt does evict the cached polish prefix once, which
  happens only on the first dictation into a new site or app. On Fairspoken
  Cloud it calls `POST /v1/format` with
  `{appName, windowTitle, host, field}` and expects `{format}`. That endpoint
  is not in this repository; until the Worker has it, the call returns 404, the
  client stops asking until restart, and dictations stay `plain`.
- **Cache:** `format-memory.json` next to settings holds `{key, format, source,
  updatedAt}` per site or app, and nothing else. A user mapping is never
  overwritten by the classifier. A timeout or failure is not cached.

### Inspecting a decision

Each dictation's history entry (`transcript-history.json`) carries
`format: {format, source, reply, rule}` when polish was on. With debug capture
on (development builds), the trace has a `format-decision` event with the
same fields plus the key, the field role, and whether a title or URL was read.
It never holds the title or the URL path.

## Streaming

Streaming polish re-polishes the tail while the user speaks and joins passes
with a space. Where a pass ends with a greeting line ("Hi Mary,") or the next
one opens with a sign-off block ("Thanks,\nSam"), the join keeps the line
break instead (`polish_stream::join`), and the tail after a greeting keeps its
capital (`transcript_cleanup::repair_fragment_edges`).
