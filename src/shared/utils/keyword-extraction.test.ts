import { describe, it, expect } from "vitest";
import {
  extractKeywords,
  calculateRelevance,
  findMatchedKeywords,
  STOP_WORDS,
} from "./keyword-extraction.js";
import type { WorkflowyNode } from "../types/index.js";

describe("extractKeywords", () => {
  it("returns empty array for empty input", () => {
    expect(extractKeywords("")).toEqual([]);
    expect(extractKeywords(null as unknown as string)).toEqual([]);
  });

  it("extracts meaningful keywords", () => {
    const text = "Machine learning algorithms for natural language processing";
    const keywords = extractKeywords(text);
    expect(keywords).toContain("machine");
    expect(keywords).toContain("learning");
    expect(keywords).toContain("algorithms");
    expect(keywords).toContain("natural");
    expect(keywords).toContain("language");
    expect(keywords).toContain("processing");
  });

  it("filters out stop words", () => {
    const text = "The quick brown fox jumps over the lazy dog";
    const keywords = extractKeywords(text);
    expect(keywords).not.toContain("the");
    expect(keywords).not.toContain("over");
    expect(keywords).toContain("quick");
    expect(keywords).toContain("brown");
    expect(keywords).toContain("fox");
    expect(keywords).toContain("jumps");
    expect(keywords).toContain("lazy");
    expect(keywords).toContain("dog");
  });

  it("filters out short words (< 3 chars)", () => {
    const text = "AI is a big topic to me";
    const keywords = extractKeywords(text);
    expect(keywords).not.toContain("ai");
    expect(keywords).not.toContain("is");
    expect(keywords).not.toContain("a");
    expect(keywords).not.toContain("to");
    expect(keywords).not.toContain("me");
    expect(keywords).toContain("big");
    expect(keywords).toContain("topic");
  });

  it("filters out pure numbers", () => {
    const text = "Project 2024 with 100 items";
    const keywords = extractKeywords(text);
    expect(keywords).not.toContain("2024");
    expect(keywords).not.toContain("100");
    expect(keywords).toContain("project");
    expect(keywords).toContain("items");
  });

  it("deduplicates keywords", () => {
    const text = "test test test repeated repeated";
    const keywords = extractKeywords(text);
    expect(keywords.filter((k) => k === "test").length).toBe(1);
    expect(keywords.filter((k) => k === "repeated").length).toBe(1);
  });

  it("handles special characters", () => {
    const text = "Hello, world! How's it going? Test@email.com";
    const keywords = extractKeywords(text);
    expect(keywords).toContain("hello");
    expect(keywords).toContain("world");
    expect(keywords).toContain("going");
  });
});

describe("calculateRelevance", () => {
  const sourceId = "source123";

  it("returns 0 for source node itself", () => {
    const node: WorkflowyNode = { id: "source123", name: "Test" };
    expect(calculateRelevance(node, ["test"], sourceId)).toBe(0);
  });

  it("returns 0 for no keyword matches", () => {
    const node: WorkflowyNode = { id: "other", name: "Completely different" };
    expect(calculateRelevance(node, ["machine", "learning"], sourceId)).toBe(0);
  });

  it("scores note matches", () => {
    const node: WorkflowyNode = {
      id: "other",
      name: "Title",
      note: "Contains machine learning content",
    };
    expect(calculateRelevance(node, ["machine", "learning"], sourceId)).toBe(2);
  });

  it("gives higher score for title matches", () => {
    const nodeWithTitleMatch: WorkflowyNode = {
      id: "title",
      name: "Machine Learning Guide",
      note: "Some content",
    };
    const nodeWithNoteMatch: WorkflowyNode = {
      id: "note",
      name: "Guide",
      note: "About machine learning",
    };

    const titleScore = calculateRelevance(
      nodeWithTitleMatch,
      ["machine", "learning"],
      sourceId
    );
    const noteScore = calculateRelevance(
      nodeWithNoteMatch,
      ["machine", "learning"],
      sourceId
    );

    expect(titleScore).toBeGreaterThan(noteScore);
  });

  it("accumulates score for multiple keyword matches", () => {
    const node: WorkflowyNode = {
      id: "multi",
      name: "Machine learning AI deep learning",
    };
    const singleKeyword = calculateRelevance(node, ["machine"], sourceId);
    const multiKeyword = calculateRelevance(
      node,
      ["machine", "learning", "deep"],
      sourceId
    );
    expect(multiKeyword).toBeGreaterThan(singleKeyword);
  });
});

describe("findMatchedKeywords", () => {
  it("returns empty array for no matches", () => {
    const node: WorkflowyNode = { id: "1", name: "Hello world" };
    expect(findMatchedKeywords(node, ["xyz", "abc"])).toEqual([]);
  });

  it("returns matched keywords from name", () => {
    const node: WorkflowyNode = { id: "1", name: "Machine learning project" };
    const matched = findMatchedKeywords(node, [
      "machine",
      "learning",
      "unrelated",
    ]);
    expect(matched).toContain("machine");
    expect(matched).toContain("learning");
    expect(matched).not.toContain("unrelated");
  });

  it("returns matched keywords from note", () => {
    const node: WorkflowyNode = {
      id: "1",
      name: "Title",
      note: "Contains important keywords here",
    };
    const matched = findMatchedKeywords(node, ["important", "keywords", "xyz"]);
    expect(matched).toContain("important");
    expect(matched).toContain("keywords");
    expect(matched).not.toContain("xyz");
  });
});

describe("STOP_WORDS", () => {
  it("contains common English stop words", () => {
    expect(STOP_WORDS.has("the")).toBe(true);
    expect(STOP_WORDS.has("and")).toBe(true);
    expect(STOP_WORDS.has("is")).toBe(true);
    expect(STOP_WORDS.has("are")).toBe(true);
  });

  it("does not contain meaningful words", () => {
    expect(STOP_WORDS.has("machine")).toBe(false);
    expect(STOP_WORDS.has("learning")).toBe(false);
    expect(STOP_WORDS.has("algorithm")).toBe(false);
  });
});
