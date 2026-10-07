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
            test_raws = {r["meta"]["raw"] for r in test}
            train_raws = {r["messages"][1]["content"].split("<transcript>")[1].split("</transcript>")[0] for r in train}
            self.assertFalse(test_raws & train_raws)
            for r in test:
                m = r["meta"]
                # Unspoken terms never reach the target.
                for t in m["unspoken_terms"]:
                    self.assertNotIn(t.lower(), m["expected"].lower())
                self.assertEqual(r["messages"][1]["content"],
                                 contract.user_message(m["raw"], m["spelling"], m["tag_format"], m["tag_tone"]))
            share = sum(r["meta"]["raw"] == r["meta"]["expected"] for r in test) / len(test)
            self.assertGreater(share, 0.25)

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
