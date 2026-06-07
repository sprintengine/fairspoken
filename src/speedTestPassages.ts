// Read-aloud passages for the speed test. Deliberately multi-sentence
// (~85–105 words): a one-liner undersells the typing grind and narrows the
// gap between the two legs. Neutral domain, comfortable to speak — no code,
// URLs, or dense punctuation that would trip either the typist or the
// transcriber. Roughly even length so runs are comparable.

export const SPEED_TEST_PASSAGES: readonly string[] = [
  "The harbor lights came on one by one as the evening fog rolled in, and the small boats turned for home before the tide could change against them. By the time the last of them reached the pier the rain had started in earnest, and the harbour master stood under the narrow awning, counting the masts as they came in and marking each one off in a notebook he had carried for longer than most of the crews had been alive.",
  "She left the office a little after six, walked the long way home past the river, and decided that whatever the meeting decided tomorrow could wait until tomorrow. The streets were quiet for a weekday, the cafes were closing one by one, and somewhere a few blocks over a band was tuning up for a show that probably would not start on time. She bought a coffee she did not need and drank it slowly, in no hurry at all to be anywhere.",
  "Most of the team agreed the new plan was simpler, though nobody could say for certain how the older systems would behave once the weekend migration finished. They had tested what they could, written down the parts they were unsure about, and agreed to meet early on Monday in case something needed a quick decision. For now there was nothing to do but wait, watch the dashboards, and trust that the work they had done carefully over the last month would hold.",
  "The train was late again, which gave her time to finish the chapter she had been carrying around for a week. The platform filled slowly with the usual evening crowd, people checking their phones and shifting their bags from one shoulder to the other. When the announcement finally came it was almost an apology, and the whole platform seemed to sigh at once before drifting toward the yellow line to wait for the doors to open.",
  "Nobody had expected the garden to do so well in its first year, least of all the people who had planted it. By midsummer the beds were crowded with tomatoes and beans, the herbs had spread well past their borders, and there were more courgettes than anyone in the building could reasonably eat. They left baskets of them in the lobby with a small handwritten sign, and somehow, by the end of each day, the baskets were always empty.",
  "The recipe had been in the family for so long that no one could say where it came from, and over the years each cook had changed it a little. Someone had added more garlic, someone else had quietly cut the sugar, and the result was a dish that tasted slightly different at every table yet somehow always the same. On cold evenings it was the first thing they reached for, and the smell of it filled the kitchen long before dinner.",
];

// Pick a passage at random, avoiding an immediate repeat when one is excluded.
export function pickPassage(exclude?: string): string {
  const pool = exclude
    ? SPEED_TEST_PASSAGES.filter((p) => p !== exclude)
    : SPEED_TEST_PASSAGES;
  const choices = pool.length > 0 ? pool : SPEED_TEST_PASSAGES;
  return choices[Math.floor(Math.random() * choices.length)];
}
