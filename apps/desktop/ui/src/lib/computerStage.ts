/**
 * What the computer card's device illustration says about the computer, as plain data: online,
 * offline or busy, plus which bot is the main one. Pure so it can be tested without a DOM.
 */
export type ComputerPresence = "online" | "offline" | "busy";

export function computerPresence(input: { offline: boolean; working: boolean }): ComputerPresence {
  if (input.offline) return "offline";
  return input.working ? "busy" : "online";
}

/** The main bot's name for the card, or null when there is nothing to say (one bot, no name). */
export function mainBotLabel(mainBotName: string | undefined): string | null {
  const name = mainBotName?.trim();
  return name ? name : null;
}
