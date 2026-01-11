/**
 * Keyword extraction and relevance scoring for knowledge linking
 */

import type { WorkflowyNode } from "../types/index.js";

/** Common stop words to filter out when extracting keywords */
export const STOP_WORDS = new Set([
  "a", "an", "the", "and", "or", "but", "in", "on", "at", "to", "for", "of",
  "with", "by", "from", "as", "is", "was", "are", "were", "been", "be", "have",
  "has", "had", "do", "does", "did", "will", "would", "could", "should", "may",
  "might", "must", "can", "this", "that", "these", "those", "i", "you", "he",
  "she", "it", "we", "they", "what", "which", "who", "whom", "when", "where",
  "why", "how", "all", "each", "every", "both", "few", "more", "most", "other",
  "some", "such", "no", "nor", "not", "only", "own", "same", "so", "than",
  "too", "very", "just", "also", "now", "here", "there", "then", "once",
  "if", "about", "into", "through", "during", "before", "after", "above",
  "below", "between", "under", "again", "further", "any", "your", "my", "our",
  "their", "its", "his", "her", "up", "down", "out", "off", "over", "under",
  "get", "got", "make", "made", "take", "took", "see", "saw", "know", "knew",
  "think", "thought", "come", "came", "go", "went", "want", "need", "use",
  "used", "like", "new", "first", "last", "long", "great", "little", "own",
  "good", "bad", "right", "left", "being", "thing", "things", "way", "ways",
  "work", "well", "even", "back", "still", "while", "since", "much", "many"
]);

/**
 * Extract significant keywords from text
 * Filters stop words, short words, and numbers
 */
export function extractKeywords(text: string): string[] {
  if (!text) return [];

  // Normalize text: lowercase, remove special chars except hyphens in words
  const normalized = text
    .toLowerCase()
    .replace(/[^\w\s-]/g, " ")
    .replace(/\s+/g, " ")
    .trim();

  // Split into words
  const words = normalized.split(" ");

  // Filter and dedupe keywords
  const keywords: string[] = [];
  const seen = new Set<string>();

  for (const word of words) {
    // Skip short words, stop words, and duplicates
    if (word.length < 3) continue;
    if (STOP_WORDS.has(word)) continue;
    if (seen.has(word)) continue;

    // Skip pure numbers
    if (/^\d+$/.test(word)) continue;

    seen.add(word);
    keywords.push(word);
  }

  return keywords;
}

/**
 * Calculate relevance score between a node and keywords
 * Title matches are weighted 3x more than note matches
 */
export function calculateRelevance(
  node: WorkflowyNode,
  keywords: string[],
  sourceNodeId: string
): number {
  // Don't match the source node itself
  if (node.id === sourceNodeId) return 0;

  const nodeText = `${node.name || ""} ${node.note || ""}`.toLowerCase();
  let score = 0;

  for (const keyword of keywords) {
    // Count occurrences of keyword in node text
    const regex = new RegExp(`\\b${keyword}\\b`, "gi");
    const matches = nodeText.match(regex);
    if (matches) {
      // Boost score for title matches vs note matches
      const titleMatches = (node.name || "").toLowerCase().match(regex);
      score += matches.length;
      if (titleMatches) {
        score += titleMatches.length * 2; // Title matches worth 3x total
      }
    }
  }

  return score;
}

/**
 * Find which keywords matched in a node's text
 */
export function findMatchedKeywords(
  node: WorkflowyNode,
  keywords: string[]
): string[] {
  const nodeText = `${node.name || ""} ${node.note || ""}`.toLowerCase();
  return keywords.filter((kw) =>
    new RegExp(`\\b${kw}\\b`, "i").test(nodeText)
  );
}
