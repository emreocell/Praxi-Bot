import { memo, type ComponentProps } from "react";
import { BotAvatar } from "ui";
import type { Bot } from "../api/types";
import { botView } from "../lib/botProfile";
import type { MessageTimes } from "../lib/messageTime";
import type { Chain } from "../lib/thinkingChain";
import { ThinkingChain } from "./ThinkingChain";
import styles from "../views/ChatView.module.css";

export const ChatStepGroup = memo(function ChatStepGroup({ chain, times, lead, bot, botId, state }: {
  chain: Chain; times: MessageTimes; lead: boolean; bot?: Bot; botId?: string;
  state?: ComponentProps<typeof BotAvatar>["state"];
}) {
  return (
    <div className={styles.chainRow}>
      <ThinkingChain chain={chain} times={times} avatar={lead ? (
        <BotAvatar bot={bot ? botView(bot) : { id: botId ?? "unknown", name: "Bot" }}
          size="xs" state={state} decorative />
      ) : undefined} />
    </div>
  );
});
