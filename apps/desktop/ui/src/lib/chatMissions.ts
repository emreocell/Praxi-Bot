/**
 * Which missions were started from which chat with `/mission`. The daemon keeps no link from a
 * mission back to the chat it was started from (a mission runs in its own chat), so this computer
 * remembers it to draw the mission's card in the chat where the owner typed the command. A mission
 * the bot started itself (`mission_create`) is found by its goal instead (see `missionsStartedIn`).
 */

const KEY = "praxi.chatMissions";
/** Oldest links are dropped beyond this many; a card is only a convenience, the panel lists every mission. */
const MAX_LINKS = 200;

export interface ChatMissionLink {
  chatId: string;
  missionId: string;
}

export function parseChatMissions(raw: unknown): ChatMissionLink[] {
  if (!Array.isArray(raw)) {
    return [];
  }
  return raw.filter(
    (x): x is ChatMissionLink =>
      typeof x === "object" && x !== null && typeof (x as ChatMissionLink).chatId === "string" && typeof (x as ChatMissionLink).missionId === "string"
  );
}

function readAll(): ChatMissionLink[] {
  try {
    return parseChatMissions(JSON.parse(localStorage.getItem(KEY) ?? "[]"));
  } catch {
    return [];
  }
}

/** The ids of the missions started from this chat with `/mission`, oldest first. */
export function chatMissionIds(chatId: string): string[] {
  return readAll()
    .filter((l) => l.chatId === chatId)
    .map((l) => l.missionId);
}

/** Add one link, keeping the newest `MAX_LINKS`. */
export function addChatMissionLink(all: readonly ChatMissionLink[], link: ChatMissionLink): ChatMissionLink[] {
  const next = [...all.filter((l) => l.missionId !== link.missionId), link];
  return next.slice(Math.max(0, next.length - MAX_LINKS));
}

export function rememberChatMission(chatId: string, missionId: string): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(addChatMissionLink(readAll(), { chatId, missionId })));
  } catch {
    // storage unavailable: the mission still runs and the panel lists it
  }
}
