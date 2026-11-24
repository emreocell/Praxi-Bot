/** Lets a card inside a dialog (e.g. the Usage page in Settings) ask the shell to open a chat. */
export const OPEN_CHAT_EVENT = "praxi:open-chat";

export interface OpenChatTarget {
  chatId: string;
}

export function requestOpenChat(chatId: string): void {
  window.dispatchEvent(new CustomEvent<OpenChatTarget>(OPEN_CHAT_EVENT, { detail: { chatId } }));
}
