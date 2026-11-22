import { type ChangeEvent, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { ProvidersDto } from "../api/types";
import { formatModelRef, resolveAutoExecutorRef } from "../lib/botProfile";
import { botWriteMessage } from "../lib/botWrite";
import {
  type ImportPreview,
  initialLevel,
  MAX_PROFILE_BYTES,
  noteIsWarning,
  type SharePreset,
  SHARE_LEVELS
} from "../lib/profileShare";
import { useDaemon } from "../state/DaemonContext";
import { Callout, DialogBody, DialogFooter, DialogHeader } from "./DialogParts";
import { Segmented, SettingGroup, SettingRow, Switch } from "./SettingsUi";
import styles from "./ProfileImportView.module.css";

export type ProfileSource =
  | { kind: "file" }
  | { kind: "template"; contents: string; requested?: SharePreset };

const CAPABILITIES = [
  ["can_run_shell", "shell"],
  ["can_use_files", "files"],
  ["can_control_desktop", "desktop"],
  ["can_use_browser", "browser"],
  ["can_run_scripts", "scripts"],
  ["can_use_own_computer", "ownComputer"]
] as const;

/**
 * Import a bot profile (a file from someone else or a gallery template): pick the highest
 * permission level it may have, read what it would be allowed to do, then create the bot. Nothing
 * is created before "Create bot", and the daemon never gives the bot more than the level chosen
 * here.
 */
export function ProfileImportView({
  source,
  providersDto,
  onBack,
  onDone
}: {
  source: ProfileSource;
  providersDto: ProvidersDto | null;
  onBack: () => void;
  onDone: (botId: string) => void;
}) {
  const { t } = useTranslation();
  const { client, conn, refreshBots } = useDaemon();
  const [contents, setContents] = useState<string | null>(
    source.kind === "template" ? source.contents : null
  );
  const [level, setLevel] = useState<SharePreset>(
    initialLevel(source.kind, source.kind === "template" ? source.requested : undefined)
  );
  const [computerFiles, setComputerFiles] = useState(false);
  const [model, setModel] = useState<string | undefined>(undefined);
  const [name, setName] = useState<string | undefined>(undefined);
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const picker = useRef<HTMLInputElement>(null);

  const auto = useMemo(
    () => formatModelRef(resolveAutoExecutorRef(providersDto, Boolean(conn?.fake_provider))),
    [providersDto, conn?.fake_provider]
  );

  useEffect(() => {
    if (!client || contents === null) return undefined;
    let cancelled = false;
    setError(null);
    client
      .previewProfile({
        contents,
        permission_level: level,
        computer_files: computerFiles,
        ...(model ? { model } : {}),
        ...(name?.trim() ? { name: name.trim() } : {})
      })
      .then((p) => {
        if (cancelled) return;
        setPreview(p);
        // The file's own model is not available here: use the app's default brain.
        if (p.model == null && !model && auto) setModel(auto);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setPreview(null);
        setError(botWriteMessage(e, t));
      });
    return () => {
      cancelled = true;
    };
  }, [client, contents, level, computerFiles, model, name, auto, t]);

  const onFile = async (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    if (file.size > MAX_PROFILE_BYTES) {
      setError(t("profileShare.tooLarge"));
      return;
    }
    setPreview(null);
    setName(undefined);
    setModel(undefined);
    setContents(await file.text());
  };

  const create = async () => {
    if (!client || contents === null || !preview || busy) return;
    setBusy(true);
    setError(null);
    try {
      const out = await client.importProfile({
        contents,
        permission_level: level,
        computer_files: computerFiles,
        ...(model ? { model } : {}),
        ...(name?.trim() ? { name: name.trim() } : {})
      });
      await refreshBots().catch(() => {});
      onDone(out.bot.id);
    } catch (e) {
      setError(botWriteMessage(e, t));
    } finally {
      setBusy(false);
    }
  };

  const open = preview?.categories.filter((c) => c.granted !== "deny") ?? [];
  const reach = preview ? CAPABILITIES.filter(([key]) => preview[key]) : [];

  return (
    <div className={styles.root} data-testid="profile-import">
      <DialogHeader
        title={t("profileShare.title")}
        subtitle={t("profileShare.subtitle")}
        closeDisabled={busy}
        onClose={onBack}
      />
      <DialogBody className={styles.body} testId="profile-import-body">
        {contents === null ? (
          <div className={styles.pick}>
            <p className={styles.hint}>{t("profileShare.pickHint")}</p>
            <Button variant="primary" onClick={() => picker.current?.click()}>
              {t("profileShare.chooseFile")}
            </Button>
          </div>
        ) : null}

        {preview ? (
          <>
            <SettingGroup>
              <SettingRow
                stacked
                title={t("profileShare.level")}
                description={t("profileShare.levelHint")}
              >
                <Segmented
                  label={t("profileShare.level")}
                  value={level}
                  options={SHARE_LEVELS.map((l) => ({
                    value: l,
                    label: t(`profileShare.levels.${l}`),
                    testId: `profile-level-${l}`
                  }))}
                  onChange={setLevel}
                />
              </SettingRow>
              <SettingRow
                title={t("profileShare.computerFiles")}
                description={t("profileShare.computerFilesHint")}
              >
                <Switch
                  checked={computerFiles}
                  label={t("profileShare.computerFiles")}
                  onChange={setComputerFiles}
                />
              </SettingRow>
            </SettingGroup>

            <section className={styles.section} aria-label={t("profileShare.willDo")}>
              <h3 className={styles.heading}>{t("profileShare.willDo")}</h3>
              <p className={styles.line} data-testid="profile-import-levels">
                {t("profileShare.levelLine", {
                  requested: t(`profileShare.levels.${preview.requested_level}`),
                  granted: t(`profileShare.levels.${preview.granted_level}`)
                })}
              </p>
              {reach.length === 0 ? (
                <p className={styles.line}>{t("profileShare.reachNone")}</p>
              ) : (
                <ul className={styles.list}>
                  {reach.map(([, key]) => (
                    <li key={key}>{t(`profileShare.reach.${key}`)}</li>
                  ))}
                </ul>
              )}
              {open.length > 0 ? (
                <ul className={styles.list} aria-label={t("profileShare.categories")}>
                  {open.map((c) => (
                    <li key={c.category}>
                      {t(`profileShare.category.${c.category}`)}:{" "}
                      {t(`profileShare.categoryLevel.${c.granted}`)}
                    </li>
                  ))}
                </ul>
              ) : null}
              <p className={styles.line}>
                {preview.all_tools
                  ? t("profileShare.toolsAll")
                  : t("profileShare.toolsOnly", { tools: preview.tools_allow.join(", ") })}
              </p>
            </section>

            {preview.notes.length > 0 ? (
              <section className={styles.section} aria-label={t("profileShare.notes")}>
                <h3 className={styles.heading}>{t("profileShare.notes")}</h3>
                <ul className={styles.list} data-testid="profile-import-notes">
                  {preview.notes.map((n, i) => (
                    <li
                      key={`${n.code}-${n.subject ?? ""}-${i}`}
                      className={noteIsWarning(n.code) ? styles.warn : undefined}
                    >
                      {t(`profileShare.note.${n.code}`, { subject: n.subject ?? "" })}
                    </li>
                  ))}
                </ul>
              </section>
            ) : null}

            {preview.skills.length > 0 ? (
              <section className={styles.section} aria-label={t("profileShare.skills")}>
                <h3 className={styles.heading}>{t("profileShare.skills")}</h3>
                <p className={styles.hint}>{t("profileShare.skillsHint")}</p>
                <ul className={styles.list}>
                  {preview.skills.map((s) => (
                    <li key={s.name}>
                      {s.name}
                      {s.version ? ` ${s.version}` : ""} (
                      {s.installed
                        ? t("profileShare.skillInstalled")
                        : t("profileShare.skillMissing")}
                      )
                    </li>
                  ))}
                </ul>
              </section>
            ) : null}

            {preview.schedule_ideas.length > 0 ? (
              <section className={styles.section} aria-label={t("profileShare.schedules")}>
                <h3 className={styles.heading}>{t("profileShare.schedules")}</h3>
                <p className={styles.hint}>{t("profileShare.schedulesHint")}</p>
                <ul className={styles.list}>
                  {preview.schedule_ideas.map((s, i) => (
                    <li key={`${s.title}-${i}`}>
                      {s.title}
                      {s.when ? ` (${s.when})` : ""}
                    </li>
                  ))}
                </ul>
              </section>
            ) : null}

            <section className={styles.section} aria-label={t("profileShare.persona")}>
              <h3 className={styles.heading}>{t("profileShare.persona")}</h3>
              <p className={styles.hint}>{t("profileShare.personaHint")}</p>
              <pre className={styles.persona} data-testid="profile-import-persona">
                {preview.persona || t("profileShare.personaEmpty")}
              </pre>
            </section>

            <div className={styles.nameRow}>
              <label htmlFor="profile-import-name" className={styles.heading}>
                {t("profileShare.name")}
              </label>
              <input
                id="profile-import-name"
                className={styles.input}
                value={name ?? preview.name}
                maxLength={80}
                onChange={(e) => setName(e.target.value)}
              />
            </div>

            {preview.model == null ? (
              <Callout tone="warning" role="status">
                {t("profileShare.noModel")}
              </Callout>
            ) : null}
          </>
        ) : null}

        {error ? (
          <Callout tone="danger" role="alert">
            {error}
          </Callout>
        ) : null}
      </DialogBody>
      <DialogFooter>
        <Button variant="ghost" disabled={busy} onClick={onBack}>
          {t("profileShare.back")}
        </Button>
        {contents !== null && source.kind === "file" ? (
          <Button variant="ghost" disabled={busy} onClick={() => picker.current?.click()}>
            {t("profileShare.otherFile")}
          </Button>
        ) : null}
        <Button
          variant="primary"
          aria-busy={busy}
          disabled={busy || !preview || preview.model == null || !preview.name.trim()}
          data-testid="profile-import-create"
          onClick={() => void create()}
        >
          {busy ? t("newBot.creating") : t("profileShare.create")}
        </Button>
      </DialogFooter>
      <input
        ref={picker}
        type="file"
        accept=".json,application/json"
        hidden
        data-testid="profile-import-file"
        onChange={(e) => void onFile(e)}
      />
    </div>
  );
}
