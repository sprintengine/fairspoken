# Polish LoRA adapters and vocabulary

How Fairspoken's local polish model learns behaviour (LoRA) and vocabulary
(retrieval), and what each can and cannot do. The training pipeline is in
[`training/polish`](../training/polish/README.md).

## Can we just feed a list of terms into an adapter?

No. A LoRA adapter is trained from **examples**, pairs of "this was said" and
"this should appear", not from lists. A list of drug names or product names
has no input→output behaviour in it, so there is nothing for training to
learn. Wrapping each term in a few made-up sentences does teach the model the
term a little, but unreliably, and it teaches the wrong lesson as well: the
model learns that the word is likely, and becomes more willing to write it
when it was *not* said. For polish that is the worst failure there is, and
for a clinical pack it is a patient-safety one (one drug name silently
becoming another).

Vocabulary therefore takes a different path that is exact, auditable and
instant to update:

1. **Retrieval.** From the dictionary and enabled packs, only the terms that
   sound like something in this transcript are picked
   (`transcript_cleanup::relevant_vocabulary`).
2. **The `<spelling>` tag.** Those terms go to the model in the tag. The
   tag-trained model has been taught, with examples of both, to use a
   spelling only for a word that was actually spoken, and never to insert a
   listed term that was not.
3. **Guards.** Whatever the model returns, `validate_output` rejects an
   output that contains a dictionary term nobody said.

Adding a term is then a settings change, not a training run, and it works
the same on every model.

## What LoRA is good at

Behaviour that is the same across many dictations:

- **Restraint**: leaving already-correct text untouched, character for
  character (40% of the training pairs need no change for this reason).
- **Format**: email layout with spoken greetings and sign-offs on their own
  lines, bullets in notes, identifiers kept verbatim in code.
- **House style**: a pack's conventions, for example the
  `ie-general-practice` adapter writing "500 milligrams" as "500 mg", UK/IE
  spellings ("anaemia", "paediatric") and clinical abbreviations spoken as
  letters ("h b a one c" → "HbA1c").
- **Your habits**: trained on your own dictations (the opt-in export in
  [training-data.md](training-data.md)), it learns how you punctuate, which
  fillers you use and what you always correct afterwards.

Adapters are a few megabytes, trained for one exact base model. The app
loads one only when its manifest names the model in use by SHA-256, and
applies it per dictation through llama-server's per-request `lora` field; on
any mismatch polish runs without it.

## What retrieval is good at

Vocabulary: names, drugs, places, product and code identifiers. Terms change
often, matter exactly, and must never appear unless they were said. A list is
the right tool for that, and the model only has to follow the tag.

## Your dictations and the speech model

The same opt-in export keeps the audio with your final edits. That is also
what is needed to improve **recognition** itself, by fine-tuning or
distilling the speech model (Parakeet, with NVIDIA NeMo) on your voice and
vocabulary. That is a separate track from this LoRA: it changes what is
heard, where polish can only change what is written from it. A polish
adapter cannot recover a word the speech model never produced; an ASR
fine-tune can. See "Learning from mistakes and personalisation" in the model
comparison spec for that plan (acoustic adaptation runs on a GPU host, with a
gate against regressions on general speech).
