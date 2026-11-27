import type { TFunction } from "i18next";
import { openSettingsTarget } from "../../lib/settingsLink";
import { VaultManager } from "../VaultManager";
import { ConnectMailCard } from "./ConnectMailCard";
import { EmailCard } from "./EmailCard";
import { EmailChannelCard } from "./EmailChannelCard";
import { MailGuardCard } from "./MailGuardCard";
import type { CardDef } from "./types";

/** Cards of the "Apps & connections" mail/vault area. */
export function mailCards(t: TFunction): CardDef[] {
  return [
    {
      id: "connect-mail",
      section: "mail",
      kind: "block",
      title: t("settingsModal.connectMail.card.title"),
      description: t("settingsModal.connectMail.card.description"),
      keywords:
        "connect email calendar guided setup wizard gmail google outlook microsoft icloud yahoo fastmail app password disconnect e-posta eposta takvim bağla bağlan kur sihirbaz postfach kalender verbinden einrichten",
      node: <ConnectMailCard />
    },
    {
      id: "vault",
      section: "mail",
      kind: "block",
      title: t("vault.title"),
      description: t("vault.subtitle"),
      keywords: "vault kasa tresor boveda password login account secret sifre hesap",
      // The Vault is a page of its own; "Manage" on the bot e-mail strip goes to the mail settings.
      fullPage: true,
      node: <VaultManager onManageEmail={() => openSettingsTarget({ page: "connections", card: "email", field: "" })} />
    },
    {
      id: "email",
      section: "mail",
      kind: "block",
      title: t("settingsModal.email.title"),
      description: t("settingsModal.email.description"),
      keywords: "mail e-posta eposta imap smtp gmail outlook yahoo icloud mailbox posta kutusu agentmail postfach",
      node: <EmailCard />
    },
    {
      id: "email-channel",
      section: "mail",
      kind: "block",
      title: t("settingsModal.emailChannel.title"),
      description: t("settingsModal.emailChannel.description"),
      keywords: "mail e-posta eposta inbox gelen kutusu posteingang channel kanal bot reply yanit antwort",
      node: <EmailChannelCard />
    },
    {
      id: "mail-guard",
      section: "mail",
      kind: "block",
      title: t("settingsModal.mailGuard.title"),
      description: t("settingsModal.mailGuard.description"),
      keywords: "mail e-posta eposta opt-out unsubscribe abonelik çık çıkış engel gönderim günlüğü send log limit sperrliste abmelden versandprotokoll",
      node: <MailGuardCard />
    }
  ];
}
