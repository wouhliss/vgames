import { describe, expect, it } from "vitest";
import { checkTree, pathProblem } from "./pathRules";

describe("pathProblem", () => {
  it("accepts ordinary paths", () => {
    for (const p of ["Game/bin/game.exe", "a", "Données/été.txt", "日本/ゲーム.bin"])
      expect(pathProblem(p)).toBeNull();
  });

  it.each([
    ["/abs", "relative"],
    ["a\\b", "relative"],
    ["a//b", "empty"],
    ["a/../b", "empty"],
    [".vgames/x", "reserved"],
    ["a/b:c", "character"],
    ["a/b?", "character"],
    ["a/tab\there", "character"],
    ["dir./x", "dot or a space"],
    ["x /y", "dot or a space"],
    ["CON", "reserved"],
    ["aux.txt", "reserved"],
    ["Lpt9.log", "reserved"],
    ["COM²", "reserved"],
    ["é", "NFC"],
    [`${"a".repeat(256)}`, "255 bytes"],
    [Array.from({ length: 65 }, () => "a").join("/"), "64 folder levels"],
    [Array.from({ length: 200 }, () => "ab").join("/"), "512 bytes"],
  ])("refuses %s", (path, reason) => {
    expect(pathProblem(path)).toMatch(new RegExp(reason));
  });
});

describe("checkTree", () => {
  it("finds case duplicates and files used as folders", () => {
    const invalid = checkTree(["Game/Data.pak", "game/data.pak", "Game/x", "Game/x/y"]);
    expect(invalid).toEqual([
      { path: "game/data.pak", reason: 'same name as "Game/Data.pak" apart from letter case' },
      { path: "Game/x", reason: 'is a file, but "Game/x/y" uses it as a folder' },
    ]);
  });
});
