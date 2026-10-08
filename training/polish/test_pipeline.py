#!/usr/bin/env python3
"""Cheap checks of the pipeline's pure-Python parts (no model, no GPU).

    python3 -I test_pipeline.py            # contract, generator, importer
    <convert venv>/python -I test_pipeline.py   # also adapter conversion and merge (needs torch)
"""
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import contract  # noqa: E402
import generate_data  # noqa: E402


class Contract(unittest.TestCase):
    def test_fixture_matches_builder(self):
        # The Rust builder is tested against the same file.
        for case in json.loads((HERE / "fixtures" / "contract.json").read_text())["cases"]:
            got = contract.user_message(case["transcript"], case["spelling"], case["format"], case["tone"])
            self.assertEqual(got, case["user"])
        self.assertEqual(json.loads((HERE / "fixtures" / "contract.json").read_text())["system"], contract.SYSTEM_PROMPT)

    def test_tag_order_and_anchor(self):
        msg = contract.user_message("hi", ["A", "B"], "email", "formal")
        self.assertEqual(msg, "<spelling>A, B</spelling>\n<format>email</format>\n<tone>formal</tone>\n"
                              "<transcript>hi</transcript>\n\nOutput only the cleaned transcript.")
        self.assertRaises(ValueError, contract.user_message, "hi", (), "poem", None)


class Generator(unittest.TestCase):
    def run_gen(self, out, *extra):
        subprocess.run([sys.executable, "-I", str(HERE / "generate_data.py"), "--out", str(out),
                        "--train", "300", "--valid", "20", "--test", "60", *extra], check=True, capture_output=True)

    def test_deterministic_and_held_out(self):
        with tempfile.TemporaryDirectory() as d:
            a, b = Path(d) / "a", Path(d) / "b"
            self.run_gen(a)
            self.run_gen(b)
            for split in ("train", "valid", "test"):
                self.assertEqual((a / f"{split}.jsonl").read_bytes(), (b / f"{split}.jsonl").read_bytes())
            test = [json.loads(l) for l in (a / "test.jsonl").read_text().splitlines()]
            train = [json.loads(l) for l in (a / "train.jsonl").read_text().splitlines()]
            valid = [json.loads(l) for l in (a / "valid.jsonl").read_text().splitlines()]
            test_raws = {r["meta"]["raw"] for r in test}
            train_raws = {generate_data.row_raw(r) for r in train}
            valid_raws = {generate_data.row_raw(r) for r in valid}
            self.assertFalse(test_raws & train_raws)
            self.assertFalse(test_raws & valid_raws)
            self.assertFalse(train_raws & valid_raws)
            # Nothing the bench scores is trained on (raw or target).
            bench = generate_data.bench_raws()
            for r in train + valid:
                self.assertNotIn(generate_data.norm(generate_data.row_raw(r)), bench)
                self.assertNotIn(generate_data.norm(r["messages"][-1]["content"]), bench)
            for r in test:
                m = r["meta"]
                # Unspoken terms never reach the target.
                for t in m["unspoken_terms"]:
                    self.assertFalse(generate_data.mentions(t, m["expected"]), (t, m["expected"]))
                self.assertEqual(r["messages"][1]["content"],
                                 contract.user_message(m["raw"], m["spelling"], m["tag_format"], m["tag_tone"]))
            share = sum(r["meta"]["raw"] == r["meta"]["expected"] for r in test) / len(test)
            self.assertGreater(share, 0.25)

    def test_bench_overlap_dropped(self):
        bench = generate_data.bench_raws()
        self.assertIn(generate_data.norm("the build is green and I merged the pull request this morning"), bench)
        rows = [generate_data.record(generate_data.Example(raw, target, "plain", "neutral", [], [], [], [], "t"))
                for raw, target in [
                    ("the build is green and i merged the pull request this morning",
                     "The build is green and I merged the pull request this morning."),  # bench raw
                    ("open settings dot json, please", "Open settings dot json and set the polish model to the two B variant."),  # bench raw as target
                    ("we should ship it on friday", "We should ship it on Friday."),
                ]]
        kept = generate_data.drop_bench_overlap(rows, bench)
        self.assertEqual([r["meta"]["raw"] for r in kept], ["we should ship it on friday"])

    def test_whole_word_terms(self):
        self.assertFalse(generate_data.mentions("AF", "Call me after lunch."))
        self.assertTrue(generate_data.mentions("AF", "History of AF."))
        self.assertTrue(generate_data.mentions("Next.js", "We use next.js here"))
        self.assertTrue(generate_data.mentions("U&Es", "Repeat the U&Es."))

    def test_prompt_example_templates_stay_in_train(self):
        groups = [generate_data.GENERAL, generate_data.EXCLAIM, generate_data.QUESTIONS, generate_data.COMMANDS,
                  generate_data.DEV, generate_data.CLINICAL, generate_data.NAMES_PLACES]
        shown = [t for g in groups for t in g if generate_data.in_prompt_examples(t)]
        self.assertIn("I think we should go with the second option.", shown)
        self.assertIn("Tell me a joke about {animal}.", shown)
        self.assertIn("The invoice is for {money} and it is due in {month}.", shown)
        for g in groups:
            train, test = generate_data.split_templates(g)
            for t in shown:
                self.assertNotIn(t, test)

    def test_pack_profile_house_style(self):
        with tempfile.TemporaryDirectory() as d:
            self.run_gen(Path(d), "--profile", "ie-general-practice")
            rows = [json.loads(l)["meta"] for l in (Path(d) / "test.jsonl").read_text().splitlines()]
            self.assertTrue(any(" mg" in r["expected"] for r in rows))
            self.assertFalse(any("milligram" in r["expected"] for r in rows))


class Importer(unittest.TestCase):
    def test_export_fixture(self):
        with tempfile.TemporaryDirectory() as d:
            out = Path(d) / "mine.jsonl"
            subprocess.run([sys.executable, "-I", str(HERE / "import_dictations.py"),
                            str(HERE / "fixtures" / "training-export-v1"), "--out", str(out),
                            "--vocabulary", str(HERE / "fixtures" / "vocabulary.txt")], check=True, capture_output=True)
            rows = [json.loads(l) for l in out.read_text().splitlines()]
            pairs = {(r["meta"]["raw"], r["messages"][2]["content"]) for r in rows}
            # raw -> edited when the user edited it, else the inserted text.
            self.assertIn(("ask cloud code to rebase the branch", "Ask Claude Code to rebase the branch."), pairs)
            self.assertIn(("Let's meet on Thursday, no, Friday at ten.", "Let's meet on Friday at ten."), pairs)
            # Terminal dictations and snippet expansions are not cleanup examples.
            self.assertFalse(any(raw in ("cargo build", "sig") for raw, _ in pairs))
            first = next(r for r in rows if r["meta"]["raw"].startswith("ask cloud"))
            self.assertIn("<spelling>Claude</spelling>\n<format>code</format>", first["messages"][1]["content"])


try:
    import numpy as np
    import torch
    from safetensors.numpy import save_file as save_np
    from safetensors.torch import load_file as load_t, save_file as save_t
except ImportError:  # the training venv has no torch
    torch = None


@unittest.skipIf(torch is None, "needs torch (use the conversion venv)")
class AdapterMath(unittest.TestCase):
    def test_peft_conversion_and_merge_match_mlx(self):
        import merge_adapter
        import mlx_to_peft
        rng = np.random.default_rng(0)
        d_in, d_out, r, scale = 6, 4, 2, 2.0
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "mlx").mkdir()
            a = rng.standard_normal((d_in, r)).astype(np.float32)
            b = rng.standard_normal((r, d_out)).astype(np.float32)
            name = "language_model.model.layers.3.self_attn.q_proj"
            save_np({f"{name}.lora_a": a, f"{name}.lora_b": b}, str(d / "mlx" / "adapters.safetensors"))
            (d / "mlx" / "adapter_config.json").write_text(json.dumps(
                {"model": "x", "lora_parameters": {"rank": r, "scale": scale, "dropout": 0.0}}))
            mlx_to_peft.convert(d / "mlx", d / "peft")
            cfg = json.loads((d / "peft" / "adapter_config.json").read_text())
            self.assertEqual(cfg["lora_alpha"], scale * r)
            (d / "base").mkdir()
            w = torch.from_numpy(rng.standard_normal((d_out, d_in)).astype(np.float32))
            hf = "model.language_model.layers.3.self_attn.q_proj.weight"
            save_t({hf: w, "model.language_model.norm.weight": torch.ones(3)}, str(d / "base" / "model.safetensors"))
            (d / "base" / "config.json").write_text("{}")
            merge_adapter.merge(d / "base", d / "peft", d / "out")
            merged = load_t(str(d / "out" / "model.safetensors"))
            x = rng.standard_normal((1, d_in)).astype(np.float32)
            mlx_y = x @ w.numpy().T + scale * (x @ a) @ b  # how mlx-lm applies the adapter
            np.testing.assert_allclose(x @ merged[hf].numpy().T, mlx_y, rtol=1e-5, atol=1e-5)
            self.assertTrue(torch.equal(merged["model.language_model.norm.weight"], torch.ones(3)))


if __name__ == "__main__":
    unittest.main()
