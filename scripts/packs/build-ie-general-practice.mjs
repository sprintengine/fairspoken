#!/usr/bin/env node
// Builds src-tauri/packs/ie-general-practice.json, the Irish general practice
// vocabulary pack.
//
//   node scripts/packs/build-ie-general-practice.mjs [--xml <latestHumanlist.xml>] [--words <word list>]
//
// Sources:
// * Medicines: the HPRA "Human Medicines - Authorised Products" XML list
//   (CC BY 4.0, https://data.gov.ie/dataset/medicines-authorised-or-transfer-pending-products).
//   Downloaded into a fresh temporary directory unless --xml is given. The
//   file is untrusted data: it is read as text and scanned with regular
//   expressions; no XML parser, DTD or entity other than the five predefined
//   ones and numeric references is processed, and nothing in it is executed.
// * Weights: the HSE PCRS "Top 100 Prescribing Products" (GMS 2023) order and
//   the HSE Medicines Management Programme preferred agents, as transcribed in
//   the product spec (PCRS publishes the list only as an interactive
//   dashboard, with no machine-readable download).
// * Irish GP systems, schemes, bodies, out-of-hours co-ops, hospitals,
//   abbreviations, names and places: curated below from the public sources in
//   the spec (eHealth Ireland, ICGP, Citizens Information, HSE, CSO).
//
// The Irish Medicines Formulary is a commercial publication and is NOT used.
//
// Brand names that are also English words are dropped (they would be
// "retrieved" whenever someone said the word); the English word list defaults
// to /usr/share/dict/words and is recorded in the pack.

import { mkdtempSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HPRA_URL = "https://assets.hpra.ie/products/xml/latestHumanlist.xml";
const HPRA_DATASET = "https://data.gov.ie/dataset/medicines-authorised-or-transfer-pending-products";
const OUTPUT = join(dirname(fileURLToPath(import.meta.url)), "../../src-tauri/packs/ie-general-practice.json");
const MAX_XML_BYTES = 64 * 1024 * 1024;

function argument(name) {
  const index = process.argv.indexOf(name);
  return index > 0 ? process.argv[index + 1] : undefined;
}

async function loadXml() {
  const given = argument("--xml");
  if (given) return readFileSync(given, "utf8");
  const directory = mkdtempSync(join(tmpdir(), "fairspoken-hpra-"));
  const path = join(directory, "latestHumanlist.xml");
  const response = await fetch(HPRA_URL, { redirect: "follow" });
  if (!response.ok) throw new Error(`HPRA download failed: HTTP ${response.status}`);
  const body = Buffer.from(await response.arrayBuffer());
  if (body.length > MAX_XML_BYTES) throw new Error(`HPRA list is unexpectedly large (${body.length} bytes)`);
  writeFileSync(path, body);
  console.error(`Downloaded ${body.length} bytes to ${path}`);
  return body.toString("utf8");
}

function decodeEntities(text) {
  return text
    .replace(/&#x([0-9a-f]+);/gi, (_, hex) => String.fromCodePoint(Number.parseInt(hex, 16)))
    .replace(/&#(\d+);/g, (_, dec) => String.fromCodePoint(Number.parseInt(dec, 10)))
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&apos;/g, "'")
    .replace(/&amp;/g, "&");
}

function field(block, tag) {
  return [...block.matchAll(new RegExp(`<${tag}>([^<]*)</${tag}>`, "g"))].map((m) =>
    decodeEntities(m[1]).replace(/\s+/g, " ").trim(),
  );
}

function parseProducts(xml) {
  if (/<!DOCTYPE|<!ENTITY/i.test(xml)) throw new Error("HPRA list contains a DTD; refusing to read it");
  const published = /datePublished="([^"]+)"/.exec(xml)?.[1] ?? "unknown";
  const products = [...xml.matchAll(/<Product>([\s\S]*?)<\/Product>/g)].map((m) => ({
    name: field(m[1], "ProductName")[0] ?? "",
    holder: field(m[1], "PAHolder")[0] ?? "",
    marketed: field(m[1], "MarketInfo")[0] === "Marketed",
    substances: field(m[1], "ActiveSubstance"),
  }));
  return { published, products };
}

function englishWords() {
  const path = argument("--words") ?? "/usr/share/dict/words";
  if (!existsSync(path)) {
    console.error(`No English word list at ${path}; brand names are not filtered against one.`);
    return { path: null, words: new Set() };
  }
  // Lowercase entries only: capitalised ones are proper nouns.
  const words = new Set(
    readFileSync(path, "utf8")
      .split(/\r?\n/)
      .filter((w) => w && w === w.toLowerCase()),
  );
  return { path, words };
}

// ── Active substances ───────────────────────────────────────

// Salt, ester and hydrate words dropped from the end of a substance name:
// GPs say "amlodipine", not "amlodipine besilate".
const SALT_WORDS = new Set(
  `hydrochloride dihydrochloride hydrobromide hydrogen sodium disodium potassium calcium magnesium
  besilate besylate maleate mesilate mesylate fumarate hemifumarate succinate tartrate bitartrate
  citrate acetate phosphate diphosphate sulfate sulphate bisulfate dihydrate monohydrate trihydrate
  hemihydrate sesquihydrate tetrahydrate pentahydrate hexahydrate heptahydrate anhydrous hyclate
  bromide chloride iodide nitrate lactate gluconate carbonate bicarbonate oxide hydroxide
  dipropionate propionate valerate butyrate furoate xinafoate fumarate palmitate decanoate
  enanthate undecanoate benzoate stearate tosilate tosylate napadisilate embonate pamoate
  medoxomil cilexetil axetil pivoxil mofetil arginine erbumine tert-butylamine meglumine
  trometamol lysine olamine dimeglumine salt ester free base as equivalent hydrate solvate hcl hci hbr`.split(/\s+/),
);

// Words that are the medicine themselves, so a name made only of them stays whole.
const CATION_WORDS = new Set(["sodium", "potassium", "calcium", "magnesium", "ferrous", "ferric", "zinc", "lithium", "aluminium", "iron"]);

// Substance names that are not things anyone dictates as a medicine: water,
// gases used as excipients, and the fragments a multi-part name leaves behind.
const SUBSTANCE_DENYLIST = new Set(
  `water air nitrogen light heavy liquid white yellow soft dibasic monobasic tribasic dimethyl benzyl
  nitric nitrous chromic stannous thallous trisodium dipotassium disodium tripotassium`.split(/\s+/),
);

// Medicines that are also everyday words: retrieved only where the
// transcript capitalises them, still medicines for the drug-swap rule.
const EVERYDAY_SUBSTANCES = new Set(
  `iron copper zinc oxygen glucose lactose sodium potassium calcium magnesium urea menthol camphor
  caffeine nicotine iodine aluminium ammonia lanolin protease lactase krypton gallium indium barium
  bismuth manganese germanium yttrium lutetium lanthanum gadolinium phenol glycerol choline biotin
  taurine helium xenon silver gold sulfur sulphur charcoal honey`.split(/\s+/),
);

function baseSubstance(raw) {
  let words = raw
    .toLowerCase()
    .replace(/\(.*?\)/g, " ")
    .split(/[\s,]+/)
    .filter(Boolean);
  if (words.length === 0) return null;
  const meaningful = (w) => !SALT_WORDS.has(w) && !CATION_WORDS.has(w);
  while (words.length > 1 && SALT_WORDS.has(words.at(-1)) && words.slice(0, -1).some(meaningful)) {
    words = words.slice(0, -1);
  }
  const name = words.join(" ");
  if (words.length > 3 || name.length > 40 || /[^a-z0-9 \-']/.test(name)) return null;
  if (!/[a-z]{3}/.test(name) || SUBSTANCE_DENYLIST.has(name)) return null;
  if (words.length === 1 && SALT_WORDS.has(name) && !CATION_WORDS.has(name)) return null;
  return name;
}

// ── Brand names ─────────────────────────────────────────────

const FORM_WORDS = new Set(
  `tablet tablets capsule capsules film-coated film coated hard soft chewable dispersible orodispersible
  effervescent prolonged-release modified-release gastro-resistant solution suspension emulsion cream
  ointment gel paste lotion spray drops eye ear nasal oral powder granules injection infusion
  concentrate syrup elixir inhaler inhalation pressurised nebuliser patch patches transdermal
  suppositories suppository pessary pessaries vaginal rectal shampoo foam medicinal gas solvent
  for and in with pre-filled prefilled syringe pen cartridge vial kit lozenge lozenges pastille
  pastilles mouthwash gum plaster implant intrauterine system liquid sachet sachets bp ph eur usp
  sugar free sugar-free junior children's infant paediatric adult extra strength plus forte retard
  xl sr mr cr la`.split(/\s+/),
);

function brandFrom(productName, substanceWords, holderWords, english) {
  const first = productName.split(/\s+/)[0]?.replace(/[®™,;:]+$/g, "");
  if (!first || /\d/.test(first) || /[^A-Za-z\-']/.test(first)) return null;
  const lower = first.toLowerCase();
  if (lower.length < 4 || FORM_WORDS.has(lower)) return null;
  if (substanceWords.has(lower) || holderWords.has(lower) || english.has(lower)) return null;
  if (lower.includes("-") && lower.split("-").some((part) => substanceWords.has(part))) return null;
  // HPRA writes many brands in capitals; dictation writes them as names.
  return first === first.toUpperCase() ? first[0] + first.slice(1).toLowerCase() : first;
}

// ── Curated content ─────────────────────────────────────────

// HSE PCRS Top 100 Prescribing Products, GMS 2023, by items (spec [I1]).
const PCRS_TOP = [
  "atorvastatin", "levothyroxine", "esomeprazole", "colecalciferol", "aspirin", "paracetamol",
  "bisoprolol", "calcium carbonate", "salbutamol", "rosuvastatin", "pantoprazole", "amlodipine",
  "co-codamol", "folic acid", "ramipril", "lansoprazole", "lercanidipine", "sertraline",
  "escitalopram", "prednisolone", "amoxicillin", "mirtazapine", "pregabalin", "zopiclone",
  "venlafaxine", "apixaban", "omeprazole", "etofenamate", "macrogol", "quetiapine", "furosemide",
  "zolpidem", "diazepam", "ferrous fumarate",
];
// HSE Medicines Management Programme preferred agents (spec [I1b]).
const MMP_PREFERRED = ["candesartan", "warfarin", "tolterodine"];
// NHSBSA Prescription Cost Analysis 2025/26, secondary source (spec [W10]).
const NHSBSA_SECONDARY = [
  "dapagliflozin", "losartan", "citalopram", "clopidogrel", "simvastatin", "tamsulosin",
  "propranolol", "amitriptyline", "beclometasone", "formoterol", "metformin",
];
// Look-alike / sound-alike pairs the drug-swap guard exists for.
const LOOK_ALIKE = ["hydroxyzine", "hydralazine", "carbamazepine", "carbimazole"];

const DRUG_SPOKEN = {
  "co-codamol": ["co codamol", "coco damol"],
  colecalciferol: ["cole calciferol", "coley calciferol"],
  levothyroxine: ["levo thyroxine"],
  esomeprazole: ["eso meprazole"],
  lercanidipine: ["lerca nidipine"],
  escitalopram: ["es citalopram"],
  mirtazapine: ["mirtaza pine"],
  macrogol: ["macro gol", "macro goal"],
};

const ALWAYS_ON = [
  "Healthlink", "Healthmail", "HbA1c", "eGFR", "atorvastatin", "levothyroxine", "esomeprazole",
  "colecalciferol", "bisoprolol", "salbutamol", "rosuvastatin", "pantoprazole", "amlodipine",
  "co-codamol", "ramipril", "lansoprazole", "lercanidipine", "sertraline", "escitalopram",
  "apixaban", "mirtazapine", "pregabalin", "GMS", "CDM", "ICGP", "PCRS", "MED1", "Socrates",
  "Helix Practice Manager", "CompleteGP",
];

const CURATED = [
  // Systems (spec [I3], [I3b]).
  ["Healthlink", "system", 1, ["health link"]],
  ["Healthmail", "system", 1, ["health mail"]],
  ["Socrates", "system", 0.8, ["so cratees"]],
  ["Helix Practice Manager", "system", 0.8, ["helix pm"]],
  ["Health One", "system", 0.8, ["health 1"]],
  ["CompleteGP", "system", 0.8, ["complete g p"]],
  // Schemes and certificates (spec [I2], [I4]).
  ["GMS", "abbreviation", 0.9],
  ["medical card", "term", 0.7],
  ["GP visit card", "term", 0.8, ["g p visit card"]],
  ["Drugs Payment Scheme", "term", 0.7],
  ["DPS", "abbreviation", 0.7],
  ["Long Term Illness", "term", 0.7],
  ["LTI", "abbreviation", 0.7],
  ["Chronic Disease Management", "term", 0.8],
  ["CDM", "abbreviation", 0.8],
  ["Opportunistic Case Finding", "term", 0.6],
  ["Preventive Programme", "term", 0.6],
  ["MED1", "identifier", 0.8, ["med one"]],
  ["IB1", "identifier", 0.6, ["i b one"]],
  // Bodies and identifiers.
  ["ICGP", "organisation", 0.8],
  ["HSE", "organisation", 0.8],
  ["PCRS", "organisation", 0.8],
  ["HPRA", "organisation", 0.6],
  ["Medical Council", "organisation", 0.6],
  ["PPSN", "identifier", 0.7, ["pps number"]],
  // Out-of-hours co-ops (spec [I5]).
  ["DDOC", "organisation", 0.6, ["d doc", "dee doc"]],
  ["SouthDoc", "organisation", 0.6, ["south doc"]],
  ["Caredoc", "organisation", 0.6, ["care doc"]],
  ["WestDoc", "organisation", 0.6, ["west doc"]],
  ["ShannonDoc", "organisation", 0.6, ["shannon doc"]],
  ["NoWDoc", "organisation", 0.6, ["now doc", "north west doc"]],
  ["NEDOC", "organisation", 0.6, ["ne doc", "n e doc"]],
  ["MIDOC", "organisation", 0.6, ["mid doc", "m i doc"]],
  // Hospitals.
  ["St James's Hospital", "organisation", 0.6, ["saint james's hospital", "st james hospital"]],
  ["Beaumont Hospital", "organisation", 0.6, ["bow mont hospital"]],
  ["Mater Hospital", "organisation", 0.6, ["mater"]],
  ["Tallaght University Hospital", "organisation", 0.6, ["tala university hospital"]],
  ["CUH", "abbreviation", 0.5],
  ["UHL", "abbreviation", 0.5],
  ["UHG", "abbreviation", 0.5],
  ["Our Lady of Lourdes Hospital", "organisation", 0.5],
  ["Connolly Hospital", "organisation", 0.5],
  // Conditions and abbreviations.
  ["T2DM", "abbreviation", 0.7, ["t two d m", "t 2 dm"]],
  ["COPD", "abbreviation", 0.8],
  ["AF", "abbreviation", 0.7],
  ["IHD", "abbreviation", 0.6],
  ["TIA", "abbreviation", 0.6, [], true],
  ["CKD", "abbreviation", 0.7],
  ["eGFR", "abbreviation", 0.8, ["e gfr"]],
  ["HbA1c", "abbreviation", 0.9, ["h b a one c", "hba one c", "hb a one c"], false, ["HbA1C"]],
  ["FBC", "abbreviation", 0.7],
  ["U&Es", "abbreviation", 0.7, ["u and es", "u and e's", "u and ees", "u and e"]],
  ["LFTs", "abbreviation", 0.7, ["lft s"]],
  ["TFTs", "abbreviation", 0.7, ["tft s"]],
  ["BP", "abbreviation", 0.6],
  ["SOB", "abbreviation", 0.5, [], true],
  ["URTI", "abbreviation", 0.6],
  ["UTI", "abbreviation", 0.7],
  ["NKDA", "abbreviation", 0.6],
  ["DNACPR", "abbreviation", 0.6, ["dna cpr"]],
  ["PRN", "abbreviation", 0.6],
  ["OD", "abbreviation", 0.5],
  ["BD", "abbreviation", 0.5],
  ["TDS", "abbreviation", 0.5],
  ["QDS", "abbreviation", 0.5],
  // Names (CSO Irish Babies' Names 2025 and common Irish names, spec [I6], [I7]).
  ["Rían", "person", 0.5, ["ree an"], false, ["Rian"]],
  ["Oisín", "person", 0.6, ["osheen", "usheen", "o sheen"], false, ["Oisin"]],
  ["Éabha", "person", 0.5, ["ay va"], false, ["Eabha"]],
  ["Fiadh", "person", 0.5, ["fee a", "fia"], false],
  ["Seán", "person", 0.6, [], false, ["Sean"]],
  ["Siobhán", "person", 0.6, ["shiv awn", "shivaun", "chevonne", "shi von"], false, ["Siobhan"]],
  ["Niamh", "person", 0.6, ["neeve", "neve", "neev"]],
  ["Caoimhe", "person", 0.6, ["keeva", "kweeva", "kee va"]],
  ["Aoife", "person", 0.6, ["eefa", "ee fa"]],
  ["Sadhbh", "person", 0.5, ["sive", "syve"]],
  ["Saoirse", "person", 0.6, ["seersha", "sir sha", "searsha", "seer sha"]],
  ["Tadhg", "person", 0.5, ["tige", "taig", "teig"]],
  ["Cian", "person", 0.5, ["key an", "kian"]],
  ["Ciarán", "person", 0.6, ["keeran", "keer awn"], false, ["Ciaran"]],
  ["Pádraig", "person", 0.6, ["paw drig", "pawdrig", "pau drig"], false, ["Padraig"]],
  ["Gráinne", "person", 0.5, ["grawn ya", "grania", "grawnya"], false, ["Grainne"]],
  ["Eoghan", "person", 0.5, ["oh in"]],
  ["Róisín", "person", 0.6, ["rosheen", "ro sheen"], false, ["Roisin"]],
  ["Méabh", "person", 0.5, ["mayv", "mave"], false, ["Meabh"]],
  ["Clodagh", "person", 0.5, ["cloda", "clo da"]],
  // Places (spec [I8]).
  ["Dún Laoghaire", "place", 0.6, ["dun leery", "dun leary", "dunleary", "done leery"], false, ["Dun Laoghaire"]],
  ["Portlaoise", "place", 0.5, ["port leash", "port leesh", "portleash"]],
  ["Naas", "place", 0.5, ["nace"]],
  ["Youghal", "place", 0.5, ["yawl"]],
  ["Drogheda", "place", 0.5, ["droh heda", "drawheda"]],
  ["Ballinasloe", "place", 0.5, ["ballina slow"]],
  ["Tallaght", "place", 0.5, ["tala", "tal at"]],
  ["Leixlip", "place", 0.5, ["lex lip", "lexlip"]],
  ["Clondalkin", "place", 0.5, ["clon dalkin"]],
  ["Clonakilty", "place", 0.5, ["clona kilty"]],
  ["Athlone", "place", 0.5, ["ath lone"]],
  ["Dundalk", "place", 0.5, ["dun dalk", "dun dork"]],
  ["Cabra", "place", 0.4],
];

// ── Assembly ────────────────────────────────────────────────

function key(term) {
  return term
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]/g, "");
}

async function main() {
  const { published, products } = parseProducts(await loadXml());
  if (products.length < 1000) throw new Error(`Only ${products.length} products parsed; the list format may have changed`);
  const { path: wordsPath, words: english } = englishWords();

  const substances = new Map(); // base name -> { marketed }
  const substanceWords = new Set();
  for (const product of products) {
    for (const raw of product.substances) {
      for (const word of raw.toLowerCase().split(/[^a-z]+/)) if (word.length > 2) substanceWords.add(word);
      const base = baseSubstance(raw);
      if (!base) continue;
      const entry = substances.get(base) ?? { marketed: false };
      entry.marketed ||= product.marketed;
      substances.set(base, entry);
    }
  }
  const holderWords = new Set(
    products.flatMap((p) => p.holder.toLowerCase().split(/[^a-z]+/).filter((w) => w.length > 2)),
  );
  const brands = new Map(); // brand -> { marketed }
  for (const product of products) {
    const brand = brandFrom(product.name, substanceWords, holderWords, english);
    if (!brand) continue;
    const entry = brands.get(brand) ?? { marketed: false };
    entry.marketed ||= product.marketed;
    brands.set(brand, entry);
  }

  const terms = new Map(); // key -> term
  const add = (term) => {
    const k = key(term.term);
    if (!k) return;
    const existing = terms.get(k);
    if (existing) {
      existing.weight = Math.max(existing.weight, term.weight);
      return;
    }
    terms.set(k, term);
  };
  const drug = (name, weight, extra = {}) => ({
    term: name,
    category: "drug",
    weight: Math.round(weight * 100) / 100,
    ...(DRUG_SPOKEN[name] ? { spoken_forms: DRUG_SPOKEN[name] } : {}),
    ...extra,
  });

  PCRS_TOP.forEach((name, rank) => add(drug(name, 1 - rank * 0.005)));
  MMP_PREFERRED.forEach((name) => add(drug(name, 0.85)));
  LOOK_ALIKE.forEach((name) => add(drug(name, 0.8)));
  NHSBSA_SECONDARY.forEach((name) => add(drug(name, 0.75)));
  const missing = [...PCRS_TOP, ...MMP_PREFERRED, ...LOOK_ALIKE, ...NHSBSA_SECONDARY].filter(
    (name) => name !== "co-codamol" && name !== "macrogol" && !substances.has(name),
  );
  if (missing.length) console.error(`Listed medicines not found as HPRA substances (kept from the lists): ${missing.join(", ")}`);

  for (const [name, info] of [...substances].sort(([a], [b]) => a.localeCompare(b))) {
    const everyday = EVERYDAY_SUBSTANCES.has(name);
    add(drug(name, info.marketed ? 0.55 : 0.45, everyday ? { ambiguous: true } : {}));
  }
  for (const [name, info] of [...brands].sort(([a], [b]) => a.localeCompare(b))) {
    add(drug(name, info.marketed ? 0.4 : 0.3));
  }
  for (const [term, category, weight, spoken = [], ambiguous = false, accept = []] of CURATED) {
    const k = key(term);
    if (terms.has(k)) terms.delete(k);
    add({
      term,
      category,
      weight,
      ...(spoken.length ? { spoken_forms: spoken } : {}),
      ...(accept.length ? { accept } : {}),
      ...(ambiguous ? { ambiguous: true } : {}),
    });
  }

  const missingAlwaysOn = ALWAYS_ON.filter((t) => !terms.has(key(t)));
  if (missingAlwaysOn.length) throw new Error(`always_on terms missing: ${missingAlwaysOn.join(", ")}`);

  const pack = {
    schema: 1,
    id: "ie-general-practice",
    name: "Irish general practice",
    description:
      "Medicines authorised in Ireland, Irish GP systems and schemes, common clinical abbreviations, and Irish names and places.",
    version: `${published.slice(0, 10).replaceAll("-", ".")}.1`,
    locale: "en-IE",
    licence: "CC-BY-4.0",
    attribution:
      `Contains information from the Health Products Regulatory Authority (HPRA) "Human Medicines - Authorised Products" list (published ${published}), ` +
      "licensed under the Creative Commons Attribution 4.0 International licence (CC BY 4.0). " +
      "Changed from the original: only brand names and active substances are kept, with strengths, forms, salts and pack sizes removed and duplicates merged. " +
      "The HPRA does not endorse this pack. The Irish Medicines Formulary is not used.",
    sources: [
      {
        name: "HPRA Human Medicines - Authorised Products (XML)",
        url: HPRA_DATASET,
        licence: "CC-BY-4.0",
        used_for: `Active substances and brand names (${products.length} authorised products, published ${published}).`,
      },
      {
        name: "HSE PCRS Top 100 Prescribing Products, GMS 2023; HSE Medicines Management Programme preferred drugs",
        url: "https://www.sspcrs.ie/portal/annual-reporting/report/pharmacy",
        licence: "Facts (names and rank order only)",
        used_for: "Weights of the most prescribed medicines.",
      },
      {
        name: "NHSBSA Prescription Cost Analysis 2025/26 (secondary)",
        url: "https://www.nhsbsa.nhs.uk/statistical-collections/prescription-cost-analysis-england/prescription-cost-analysis-england-202526",
        licence: "Facts (names only)",
        used_for: "A few common medicines missing from the Irish list.",
      },
      {
        name: "Curated Irish GP terms (eHealth Ireland, ICGP, Citizens Information, HSE, CSO)",
        url: "",
        licence: "CC-BY-4.0",
        used_for: "Systems, schemes, bodies, out-of-hours co-ops, hospitals, abbreviations, names and places.",
      },
    ],
    generator: {
      script: "scripts/packs/build-ie-general-practice.mjs",
      english_word_filter: wordsPath ?? "none",
    },
    always_on: ALWAYS_ON,
    terms: [...terms.values()],
    corrections: [],
    snippets: [],
  };
  writeFileSync(OUTPUT, `${JSON.stringify(pack, null, 1)}\n`);
  const count = (category) => pack.terms.filter((t) => t.category === category).length;
  console.error(
    `Wrote ${OUTPUT}: ${pack.terms.length} terms (${substances.size} substances, ${brands.size} brands, ${count("drug")} drug entries in all).`,
  );
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
});
