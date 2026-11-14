import { useState } from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button, Card } from "ui";
import type { Bot } from "../api/types";
import {
  BotAppsFields,
  BotAvatarUploadField,
  BotModelRolesFields,
  BotPermissionsFields,
  BotRuntimeFields,
  useBotFormDeps
} from "../components/BotAdvancedFields";
import { CharacterColorRow, CharacterShapeGrid } from "../components/CharacterPicker";
import formStyles from "../components/Form.module.css";
import {
  type BotDraft,
  configFromDraft,
  draftFromBot
} from "../lib/botProfile";
import { botHasAccess, changedApps, saveBotAndApps } from "../lib/mcpApps";
import { useDaemon } from "../state/DaemonContext";
import styles from "./BotFormView.module.css";

/** Full-page editor for inspecting or updating an existing bot profile. */
export function BotFormView({
  botId,
  onDone,
  onCancel
}: {
  /** `undefined` = create a new bot. */
  botId?: string;
  onDone: (botId: string) => void;
  onCancel: () => void;
}) {
  const { bots } = useDaemon();
  const existing = bots.find((b) => b.id === botId);
  return (
    <BotFormBody
      key={existing?.id ?? (botId ? "pending" : "new")}
      botId={botId}
      existing={existing}
      onDone={onDone}
      onCancel={onCancel}
    />
  );
}

function BotFormBody({
  botId,
  existing,
  onDone,
  onCancel
}: {
  botId?: string;
  existing: Bot | undefined;
  onDone: (botId: string) => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const { client, bots, conn, refreshBots } = useDaemon();
  const {
    providersDto,
    providerIds,
    httpProviders,
    servers,
    setServers,
    allowed,
    setAllowed,
    initialApps,
    setInitialApps
  } = useBotFormDeps(client, existing?.id, Boolean(conn?.fake_provider), true);

  // The bot this form created: a retry after a partial failure must update
  // THIS bot (kept whole, so it does not depend on the bot list refreshing).
  const [createdBot, setCreatedBot] = useState<Bot | null>(null);
  const [draft, setDraft] = useState<BotDraft>(() => draftFromBot(existing));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const patch = (p: Partial<BotDraft>) => setDraft((d) => ({ ...d, ...p }));
  const nameMissing = draft.name.trim() === "";
  const executorMissing = draft.executor.model.trim() === "";

  const save = async () => {
    if (!client || nameMissing || executorMissing) {
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const target = existing ?? createdBot ?? undefined;
      const config = configFromDraft(draft, target);
      const input = { name: draft.name.trim(), config };
      const { bot, failed, notApplied } = await saveBotAndApps(
        client,
        target,
        input,
        initialApps,
        allowed,
        bots.map((b) => b.id)
      );
      if (!target) {
        setCreatedBot(bot);
      }
      await refreshBots().catch(() => {});
      if (failed.length > 0 || notApplied.length > 0) {
        const fresh = await client.listMcpServers().catch(() => null);
        const problem = new Set([...failed, ...notApplied]);
        const actual = (name: string): boolean | undefined => {
          const srv = fresh?.find((x) => x.name === name);
          return srv ? botHasAccess(srv, bot.id) : undefined;
        };
        if (fresh) {
          setServers(fresh);
        }
        setAllowed((prev) => {
          const next = { ...prev };
          for (const name of notApplied) {
            next[name] = actual(name) ?? prev[name];
          }
          return next;
        });
        setInitialApps((prev) => {
          const next = { ...prev };
          for (const name of changedApps(prev, allowed)) {
            if (notApplied.includes(name)) {
              next[name] = actual(name) ?? prev[name];
            } else if (!problem.has(name)) {
              next[name] = allowed[name];
            }
          }
          return next;
        });
        setError(
          [
            failed.length > 0
              ? t("botForm.apps.partialFail", { names: failed.join(", ") })
              : "",
            notApplied.length > 0
              ? t("botForm.apps.notApplied", { names: notApplied.join(", ") })
              : ""
          ]
            .filter(Boolean)
            .join(" ")
        );
        return;
      }
      onDone(bot.id);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className={styles.pane}>
      <div className={styles.container}>
        <header className={styles.header}>
          <h1 className={styles.title}>
            {existing ? t("botForm.editTitle") : t("botForm.createTitle")}
          </h1>
        </header>

        <Card padding="md" className={styles.section}>
          <h2 className={styles.heading}>{t("botForm.identity")}</h2>
          <div className={formStyles.row}>
            <BotAvatar
              bot={{
                id: botId ?? "new-bot",
                name: draft.name,
                shape: draft.shape,
                color: draft.color,
                avatar: draft.avatar
              }}
              size="lg"
            />
            <div className={`${formStyles.field} ${formStyles.grow}`}>
              <label className={formStyles.label} htmlFor="bot-name">
                {t("botForm.name")} *
              </label>
              <input
                id="bot-name"
                className={formStyles.input}
                value={draft.name}
                maxLength={60}
                onChange={(e) => patch({ name: e.target.value })}
              />
              {nameMissing ? (
                <p className={formStyles.hint}>{t("botForm.nameRequired")}</p>
              ) : null}
            </div>
          </div>

          <div className={formStyles.field}>
            <span className={formStyles.label}>{t("newBot.shapeGroup")}</span>
            <CharacterShapeGrid
              value={draft.shape}
              color={draft.color}
              onChange={(shape) => patch({ shape })}
            />
          </div>

          <div className={formStyles.field}>
            <span className={formStyles.label}>{t("botForm.color")}</span>
            <CharacterColorRow
              value={draft.color}
              centered={false}
              onChange={(color) => patch({ color })}
            />
          </div>

          <BotAvatarUploadField
            avatar={draft.avatar}
            onChange={(avatar) => patch({ avatar })}
          />

          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor="bot-persona">
              {t("botForm.persona")}
            </label>
            <textarea
              id="bot-persona"
              className={formStyles.textarea}
              value={draft.persona}
              placeholder={t("botForm.personaPlaceholder")}
              onChange={(e) => patch({ persona: e.target.value })}
            />
          </div>
        </Card>

        <Card padding="md" className={styles.section}>
          <h2 className={styles.heading}>{t("botForm.models")}</h2>
          <BotModelRolesFields
            draft={draft}
            providerIds={providerIds}
            httpProviders={httpProviders}
            providersDto={providersDto}
            executorMissing={executorMissing}
            patch={patch}
          />
        </Card>

        <Card padding="md" className={styles.section}>
          <h2 className={styles.heading}>{t("botForm.permissions")}</h2>
          <BotPermissionsFields draft={draft} patch={patch} />
        </Card>

        <Card padding="md" className={styles.section}>
          <h2 className={styles.heading}>{t("botForm.apps.title")}</h2>
          <BotAppsFields
            servers={servers}
            allowed={allowed}
            onToggle={(name, checked) =>
              setAllowed((a) => ({ ...a, [name]: checked }))
            }
          />
        </Card>

        <Card padding="md" className={styles.section}>
          <h2 className={styles.heading}>{t("botForm.runtime")}</h2>
          <BotRuntimeFields draft={draft} patch={patch} />
        </Card>

        {error ? (
          <p className={styles.alert} role="alert">
            {error}
          </p>
        ) : null}
        <div className={styles.footer}>
          <Button variant="secondary" onClick={onCancel}>
            {t("common.cancel")}
          </Button>
          <Button
            variant="primary"
            disabled={saving || nameMissing || executorMissing}
            onClick={() => void save()}
          >
            {existing ? t("common.save") : t("botForm.create")}
          </Button>
        </div>
      </div>
    </div>
  );
}
