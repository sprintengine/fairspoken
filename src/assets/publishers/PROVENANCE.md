# Publisher artwork

These bundled files identify the publisher of a selectable model, next to its
name. They are not Fairspoken branding and do not imply a partnership or
endorsement. The app makes no image requests to third-party hosts at runtime.
Artwork is unmodified, with no CSS tint, filter or opacity treatment. Copyright
and trademarks belong to their respective owners; the app's MIT license does
not relicense these marks.

Verified 2026-09-13.

| File | Primary source | Usage basis and presentation |
| --- | --- | --- |
| `openai.png` | [Official OpenAI organization](https://github.com/openai), [original avatar](https://avatars.githubusercontent.com/u/14957082?v=4) | OpenAI's [brand guidelines and Marks usage terms](https://openai.com/brand/#usage-terms) provide limited, revocable permission subject to the guidelines. Used only to identify OpenAI as Whisper's publisher. The original black Blossom and white plate remain intact in both themes. Copyright and OpenAI Marks remain OpenAI's. |
| `qwen.svg` | [Qwen's official repository, pinned source](https://github.com/QwenLM/qwen-code/blob/6a211b465227dd35ad3c35a2dcb86a809590bcd0/packages/desktop-shell/bootstrap/qwen-code-logo.svg) | Unmodified purple Qwen mark from the official repository. Its upstream Apache 2.0 license is included as `QWEN-LICENSE.txt`; section 6 does not grant general trademark rights. This use identifies the origin of Qwen models; it does not suggest sponsorship or apply the mark to this app's identity. Alibaba/Qwen retains the mark. |

## NVIDIA

NVIDIA is named accurately in the Parakeet model row, with a neutral model glyph.
No NVIDIA artwork is bundled. Its [official logo terms](https://www.nvidia.com/en-us/about-nvidia/legal-info/logo-brand-usage/)
require express written authorization, prohibit separating the eye and wordmark,
and impose minimum sizing. No such authorization was available for this change.
The registry can receive authorized artwork later without changing consumers.
Do not substitute a scraped eye-only logo or infer trademark permission from
Parakeet's model license.

## Fallback

NVIDIA stays on the same neutral layer glyph as the app's Models navigation:
there is no bundled NVIDIA mark, and a letter chip would still be invented
branding. Every other unknown publisher uses the kit extension-icon monogram —
one or two letters from the name — so Hub and catalog rows stay distinguishable
without fetching remote artwork. Visible publisher text remains the identity
source for assistive technology.
