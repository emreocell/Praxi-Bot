import { ArrowLeft, CalendarDays, CircleAlert, CircleCheck, ExternalLink, Mail, PlugZap } from "lucide-react";
import { type FormEvent, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { ApiClient } from "../api/client";
import type { Bot, EmailTestReport } from "../api/types";
import { BotAccessPicker } from "../components/BotAccessPicker";
import { Callout, DialogBody, DialogFooter, DialogHeader, Field } from "../components/DialogParts";
import formStyles from "../components/Form.module.css";
import { Modal } from "../components/Modal";
import { MAIL_CHANGED_EVENT, mailToolsOn, mailToolsPatch } from "../lib/botMail";
import { botFromScope } from "../lib/emailAccounts";
import { openExternal } from "../lib/externalLink";
import { humanLine } from "../lib/humanError";
import type { BotScope } from "../lib/mcpApps";
import {
  allPassed,
  anyPassed,
  type ConnectPath,
  calendarFixKey,
  calendarProblem,
  detectProvider,
  domainOf,
  type GuidePlan,
  type GuideSaved,
  type GuideTest,
  googleMailCalendarEntries,
  googleSignedIn,
  isAddress,
  isServerProblem,
  type MailServers,
  mailFixKey,
  mailServers,
  normaliseServerAddress,
  PROVIDER_IDS,
  type ProviderId,
  providerProfile,
  savePlan,
  testPlan
} from "../lib/mailGuide";
import { openApps } from "../lib/settingsLink";
import styles from "./ConnectMail.module.css";

const K = "settingsModal.connectMail";
const SUBTITLE_ID = "connect-mail-subtitle";

type Step = "address" | "setup" | "done";

/** What the flow starts with. The app only sets `address`; tests start in the middle of it. */
export interface FlowSeed {
  step?: Step;
  address?: string;
  provider?: ProviderId;
  wantMail?: boolean;
  wantCalendar?: boolean;
  googlePath?: "google_sign_in" | "app_password";
  password?: string;
  test?: GuideTest | null;
  saved?: GuideSaved | null;
  allowSend?: boolean;
  signedIn?: { google?: boolean; microsoft?: boolean };
}

type FlowClient = Pick<
  ApiClient,
  | "testEmailDraft"
  | "testCalendar"
  | "createEmailAccount"
  | "createCalendarAccount"
  | "listEmailAccounts"
  | "listCalendarAccounts"
  | "listMcpServers"
  | "mcpCatalog"
  | "composioApps"
  | "patchBot"
>;

/** Opens the page of the provider, in the system browser. */
function Link({ href, children }: { href: string; children: string }) {
  return (
    <Button size="sm" variant="secondary" data-testid="connect-mail-open-link" onClick={() => openExternal(href)}>
      {children}
      <ExternalLink aria-hidden="true" />
    </Button>
  );
}

export function ConnectMailFlow({
  client,
  bots,
  seed,
  onClose,
  onChanged
}: {
  client: FlowClient | null;
  bots: Bot[];
  seed?: FlowSeed;
  onClose: () => void;
  /** Something was connected: lists elsewhere reload. */
  onChanged?: () => void;
}) {
  const { t } = useTranslation();
  const [step, setStep] = useState<Step>(seed?.step ?? "address");
  const [address, setAddress] = useState(seed?.address ?? "");
  const [override, setOverride] = useState<ProviderId | null>(seed?.provider ?? null);
  const detected = detectProvider(address);
  const provider: ProviderId | null = override ?? detected;
  const [wantMail, setWantMail] = useState(seed?.wantMail ?? true);
  const [wantCalendar, setWantCalendar] = useState(seed?.wantCalendar ?? true);
  const [googlePath, setGooglePath] = useState<"google_sign_in" | "app_password">(seed?.googlePath ?? "google_sign_in");
  const [password, setPassword] = useState(seed?.password ?? "");
  const [login, setLogin] = useState("");
  const [serverEdit, setServerEdit] = useState<Partial<Record<keyof MailServers, string>>>({});
  const [calendarServer, setCalendarServer] = useState("");
  const [scope, setScope] = useState<BotScope>(() =>
    bots.length === 1 ? { mode: "some", ids: [bots[0].id] } : { mode: "some", ids: [] }
  );
  const [allowSend, setAllowSend] = useState(seed?.allowSend ?? false);
  const [test, setTest] = useState<GuideTest | null>(seed?.test ?? null);
  const [testing, setTesting] = useState(false);
  const [picked, setPicked] = useState<string[] | null>(null);
  const [saved, setSaved] = useState<GuideSaved | null>(seed?.saved ?? null);
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [signedIn, setSignedIn] = useState<{ google?: boolean; microsoft?: boolean }>(seed?.signedIn ?? {});
  const [checking, setChecking] = useState(false);
  const [toolsOn, setToolsOn] = useState<boolean | null>(null);

  const profile = provider ? providerProfile(provider) : null;
  const calendarPossible = profile ? profile.calendar[0] !== "none" : true;
  const mailPath: ConnectPath | null =
    profile && wantMail ? (provider === "google" ? googlePath : profile.mail[0]) : null;
  const calendarPath: ConnectPath | null = profile && wantCalendar && calendarPossible ? profile.calendar[0] : null;
  const usesPassword = mailPath === "app_password" || calendarPath === "app_password";
  const needsGoogle = mailPath === "google_sign_in" || calendarPath === "google_sign_in";
  const needsMicrosoft = mailPath === "microsoft_sign_in" || calendarPath === "microsoft_sign_in";

  const servers: MailServers | null = useMemo(() => {
    if (!provider) return null;
    const base = mailServers(provider, address);
    return {
      imapHost: serverEdit.imapHost ?? base.imapHost,
      imapPort: Number(serverEdit.imapPort ?? base.imapPort) || base.imapPort,
      smtpHost: serverEdit.smtpHost ?? base.smtpHost,
      smtpPort: Number(serverEdit.smtpPort ?? base.smtpPort) || base.smtpPort,
      smtpSecurity: (serverEdit.smtpSecurity as MailServers["smtpSecurity"] | undefined) ?? base.smtpSecurity
    };
  }, [provider, address, serverEdit]);

  const bot = botFromScope(scope);
  const plan: GuidePlan | null =
    provider && servers
      ? {
          provider,
          address: address.trim(),
          login,
          password,
          mail: { on: mailPath === "app_password", servers, bot: bot ?? null, allowSend },
          calendar: { on: calendarPath === "app_password", server: calendarServer }
        }
      : null;

  const reset = () => {
    setTest(null);
    setPicked(null);
    setProblem(null);
  };

  const missing: "password" | "bot" | "server" | null = !usesPassword
    ? null
    : password === ""
      ? "password"
      : mailPath === "app_password" && bot === undefined
        ? "bot"
        : calendarPath === "app_password" &&
            provider === "other" &&
            normaliseServerAddress(calendarServer) === ""
          ? "server"
          : null;

  const runTest = async () => {
    if (!client || !plan || missing || testing) return;
    setTesting(true);
    setProblem(null);
    try {
      const result = await testPlan(client, plan);
      setTest(result);
      const found = result.calendar?.state === "ok" ? result.calendar.report.calendars.map((c) => c.url) : [];
      setPicked(found);
    } catch (e) {
      setProblem(humanLine(e));
    } finally {
      setTesting(false);
    }
  };

  const connect = async () => {
    if (!client || !plan || !test || saving) return;
    setSaving(true);
    setProblem(null);
    try {
      const [mail, calendars] = await Promise.all([
        client.listEmailAccounts().catch(() => []),
        client.listCalendarAccounts().catch(() => [])
      ]);
      const found = test.calendar?.state === "ok" ? test.calendar.report.calendars : [];
      const chosen = found.filter((c) => (picked ?? []).includes(c.url));
      const result = await savePlan(
        client,
        plan,
        test,
        { mail: mail.map((a) => a.name), calendar: calendars.map((a) => a.name) },
        chosen
      );
      setSaved(result);
      if (result.mail || result.calendars.length > 0) {
        setPassword("");
        window.dispatchEvent(new Event(MAIL_CHANGED_EVENT));
        onChanged?.();
        setStep("done");
        const boundBot = bots.find((b) => b.id === plan.mail?.bot);
        setToolsOn(result.mail && boundBot ? mailToolsOn(boundBot.config) : null);
      } else if (result.failed.length > 0) {
        setProblem(humanLine(result.failed[0].error));
      }
    } catch (e) {
      setProblem(humanLine(e));
    } finally {
      setSaving(false);
    }
  };

  const checkSignIn = async () => {
    if (!client) return;
    setChecking(true);
    try {
      const [servers, catalog] = await Promise.all([
        client.listMcpServers().catch(() => []),
        client.mcpCatalog().catch(() => [])
      ]);
      const ids = googleMailCalendarEntries(catalog).map((e) => e.id);
      const apps = await client.composioApps().catch(() => []);
      setSignedIn({
        google: googleSignedIn(servers, ids),
        microsoft: apps.some((a) => a.slug === "outlook" && a.state === "active" && a.enabled)
      });
    } finally {
      setChecking(false);
    }
  };

  const turnOnTools = async () => {
    const boundBot = bots.find((b) => b.id === bot);
    if (!client || !boundBot) return;
    const patch = mailToolsPatch(boundBot, true);
    try {
      if (patch.ops.length > 0) await client.patchBot(boundBot.id, patch);
      setToolsOn(true);
      window.dispatchEvent(new Event(MAIL_CHANGED_EVENT));
    } catch (e) {
      setProblem(humanLine(e));
    }
  };

  const next = (e: FormEvent) => {
    e.preventDefault();
    if (provider && isAddress(address) && (wantMail || wantCalendar)) {
      reset();
      setStep("setup");
    }
  };

  const providerName = provider ? t(`${K}.providers.${provider}`) : "";
  const botName = bots.find((b) => b.id === bot)?.name ?? "";
  const title = t(`${K}.title`);

  return (
    <form
      className={styles.form}
      data-testid="connect-mail"
      data-step={step}
      onSubmit={step === "address" ? next : (e) => e.preventDefault()}
      noValidate
    >
      <DialogHeader
        title={title}
        subtitle={t(`${K}.hints.${step}`)}
        subtitleId={SUBTITLE_ID}
        closeDisabled={testing || saving}
        onClose={onClose}
      />
      <DialogBody className={styles.body} testId="connect-mail-body">
        {step === "address" ? (
          <AddressStep
            address={address}
            onAddress={(a) => {
              setAddress(a);
              setOverride(null);
            }}
            provider={provider}
            detected={detected}
            onProvider={setOverride}
            wantMail={wantMail}
            wantCalendar={wantCalendar}
            calendarPossible={calendarPossible}
            onWant={(m, c) => {
              setWantMail(m);
              setWantCalendar(c);
            }}
          />
        ) : null}

        {step === "setup" && provider && profile ? (
          <>
            <p className={styles.text} data-testid="connect-mail-summary">
              {t(`${K}.setup.for`, { address: address.trim(), provider: providerName })}
            </p>

            {wantCalendar && !calendarPossible ? (
              <Callout tone="info">{t(`${K}.calendarNone.${provider}`)}</Callout>
            ) : null}

            {provider === "google" && wantMail ? (
              <div className={styles.section}>
                <h3 className={styles.sectionTitle}>{t(`${K}.google.pathLabel`)}</h3>
                <div className={styles.choices} role="radiogroup" aria-label={t(`${K}.google.pathLabel`)}>
                  {(["google_sign_in", "app_password"] as const).map((p) => (
                    <label key={p} className={[styles.choice, googlePath === p ? styles.choiceOn : ""].join(" ")}>
                      <input
                        type="radio"
                        name="google-path"
                        checked={googlePath === p}
                        data-testid={`connect-mail-google-${p}`}
                        onChange={() => {
                          setGooglePath(p);
                          reset();
                        }}
                      />
                      <span>
                        {t(`${K}.google.paths.${p}`)}
                        <span className={styles.choiceHint}>{t(`${K}.google.paths.${p}Hint`)}</span>
                      </span>
                    </label>
                  ))}
                </div>
              </div>
            ) : null}

            {usesPassword ? (
              <div className={styles.section} data-testid="connect-mail-password-step">
                <h3 className={styles.sectionTitle}>{t(`${K}.appPassword.title`)}</h3>
                <ol className={styles.steps}>
                  <li>{t(`${K}.appPassword.${provider}.one`)}</li>
                  <li>{t(`${K}.appPassword.${provider}.two`)}</li>
                </ol>
                {profile.appPasswordUrl ? (
                  <div className={styles.row}>
                    <Link href={profile.appPasswordUrl}>{t(`${K}.appPassword.open`, { provider: providerName })}</Link>
                  </div>
                ) : null}
                <Field
                  label={t(`${K}.appPassword.label`)}
                  htmlFor="connect-mail-password"
                  hint={t(`${K}.appPassword.hint`)}
                >
                  <input
                    id="connect-mail-password"
                    className={formStyles.input}
                    type="password"
                    value={password}
                    autoComplete="new-password"
                    spellCheck={false}
                    data-testid="connect-mail-password"
                    onChange={(e) => {
                      setPassword(e.target.value);
                      reset();
                    }}
                  />
                </Field>
              </div>
            ) : null}

            {mailPath === "app_password" ? (
              <div className={styles.section}>
                <Field label={t(`${K}.bot.label`)} hint={t(`${K}.bot.hint`)}>
                  <BotAccessPicker
                    single
                    scope={scope}
                    bots={bots}
                    onChange={(s) => {
                      setScope(s);
                      reset();
                    }}
                    texts={{
                      label: t(`${K}.bot.label`),
                      all: t("settingsModal.email.form.access.all"),
                      some: t("settingsModal.email.form.access.some"),
                      nobody: t("settingsModal.email.form.access.nobody")
                    }}
                  />
                </Field>
                <div className={styles.choices}>
                  <span className={styles.sectionTitle}>{t(`${K}.send.label`)}</span>
                  <p className={styles.text} data-testid="connect-mail-readonly">
                    {t(`${K}.send.readOnly`)}
                  </p>
                  <label className={styles.choice}>
                    <input
                      type="checkbox"
                      checked={allowSend}
                      data-testid="connect-mail-allow-send"
                      onChange={(e) => {
                        setAllowSend(e.target.checked);
                        reset();
                      }}
                    />
                    <span>
                      {t(`${K}.send.allow`)}
                      <span className={styles.choiceHint}>{t(`${K}.send.allowHint`)}</span>
                    </span>
                  </label>
                </div>
              </div>
            ) : null}

            {calendarPath === "app_password" ? (
              <div className={styles.section}>
                {provider === "other" ? (
                  <Field
                    label={t(`${K}.calendarServer.label`)}
                    htmlFor="connect-mail-caldav"
                    hint={t(`${K}.calendarServer.hint`)}
                  >
                    <input
                      id="connect-mail-caldav"
                      className={formStyles.input}
                      value={calendarServer}
                      placeholder="https://caldav.example.com/"
                      autoComplete="off"
                      spellCheck={false}
                      onChange={(e) => {
                        setCalendarServer(e.target.value);
                        reset();
                      }}
                    />
                  </Field>
                ) : null}
                <p className={styles.text}>{t(`${K}.calendarNote`)}</p>
              </div>
            ) : null}

            {usesPassword && servers ? (
              <details
                className={styles.more}
                open={test?.mail?.state === "failed" && isServerProblem(worstOf(test.mail.report))}
                data-testid="connect-mail-more"
              >
                <summary>{t(`${K}.more.title`)}</summary>
                <Field label={t(`${K}.more.login`)} htmlFor="connect-mail-login" hint={t(`${K}.more.loginHint`)}>
                  <input
                    id="connect-mail-login"
                    className={formStyles.input}
                    value={login}
                    autoComplete="off"
                    placeholder={address.trim()}
                    onChange={(e) => {
                      setLogin(e.target.value);
                      reset();
                    }}
                  />
                </Field>
                {mailPath === "app_password" ? (
                  <div className={styles.grid}>
                    {(
                      [
                        ["imapHost", "settingsModal.email.form.imapHost"],
                        ["imapPort", "settingsModal.email.form.port"],
                        ["smtpHost", "settingsModal.email.form.smtpHost"],
                        ["smtpPort", "settingsModal.email.form.port"]
                      ] as const
                    ).map(([key, label]) => (
                      <Field key={key} label={t(label)} htmlFor={`connect-mail-${key}`}>
                        <input
                          id={`connect-mail-${key}`}
                          className={[formStyles.input, key.endsWith("Port") ? styles.port : ""].join(" ")}
                          value={String(servers[key])}
                          autoComplete="off"
                          spellCheck={false}
                          onChange={(e) => {
                            setServerEdit((s) => ({ ...s, [key]: e.target.value }));
                            reset();
                          }}
                        />
                      </Field>
                    ))}
                  </div>
                ) : null}
              </details>
            ) : null}

            {needsGoogle ? (
              <SignInBlock
                kind="google"
                done={signedIn.google === true}
                checked={signedIn.google !== undefined}
                checking={checking}
                onOpen={() => openApps({ advanced: true })}
                onCheck={() => void checkSignIn()}
              />
            ) : null}
            {needsMicrosoft ? (
              <SignInBlock
                kind="microsoft"
                done={signedIn.microsoft === true}
                checked={signedIn.microsoft !== undefined}
                checking={checking}
                onOpen={() => openApps({ query: "outlook" })}
                onCheck={() => void checkSignIn()}
              />
            ) : null}

            {test && plan ? (
              <TestResults
                provider={provider}
                test={test}
                picked={picked ?? []}
                onPick={(urls) => setPicked(urls)}
                allowSend={allowSend}
              />
            ) : null}

            {usesPassword && missing ? (
              <p className={styles.detail} data-testid="connect-mail-missing">
                {t(`${K}.missing.${missing}`)}
              </p>
            ) : null}

            {problem ? (
              <Callout tone="danger" role="alert">
                {problem}
              </Callout>
            ) : null}
          </>
        ) : null}

        {step === "done" ? (
          <Done
            provider={provider}
            saved={saved}
            allowSend={allowSend}
            botName={botName}
            signedIn={signedIn}
            toolsOn={toolsOn}
            onTurnOn={() => void turnOnTools()}
            accessUrl={profile?.accessUrl ?? null}
            problem={problem}
          />
        ) : null}
      </DialogBody>

      <DialogFooter>
        {step === "address" ? (
          <>
            <Button type="button" variant="ghost" onClick={onClose}>
              {t("common.cancel")}
            </Button>
            <Button
              type="submit"
              variant="primary"
              disabled={!provider || !isAddress(address) || (!wantMail && !wantCalendar)}
              data-testid="connect-mail-next"
            >
              {t(`${K}.next`)}
            </Button>
          </>
        ) : null}
        {step === "setup" ? (
          <>
            <Button
              type="button"
              variant="ghost"
              disabled={testing || saving}
              onClick={() => {
                reset();
                setStep("address");
              }}
            >
              <ArrowLeft />
              {t(`${K}.back`)}
            </Button>
            {usesPassword ? (
              <Button
                type="button"
                variant={test && anyPassed(test) ? "secondary" : "primary"}
                disabled={testing || saving || missing !== null || client === null}
                data-testid="connect-mail-test"
                onClick={() => void runTest()}
              >
                <PlugZap />
                {testing ? t(`${K}.testing`) : t(`${K}.test`)}
              </Button>
            ) : null}
            {usesPassword && test && anyPassed(test) ? (
              <Button
                type="button"
                variant="primary"
                disabled={saving}
                data-testid="connect-mail-connect"
                onClick={() => void connect()}
              >
                {saving ? t(`${K}.saving`) : allPassed(test) ? t(`${K}.connect`) : t(`${K}.connectWhatWorks`)}
              </Button>
            ) : null}
            {!usesPassword ? (
              <Button
                type="button"
                variant="primary"
                data-testid="connect-mail-finish"
                onClick={() => {
                  setSaved({ mail: null, calendars: [], failed: [] });
                  setStep("done");
                }}
              >
                {t(`${K}.finish`)}
              </Button>
            ) : null}
          </>
        ) : null}
        {step === "done" ? (
          <Button type="button" variant="primary" data-testid="connect-mail-close" onClick={onClose}>
            {t(`${K}.close`)}
          </Button>
        ) : null}
      </DialogFooter>
    </form>
  );
}

/** The worst of the two mail servers' statuses, for deciding whether server details should open. */
function worstOf(report: EmailTestReport) {
  return report.imap !== "ok" ? report.imap : report.smtp;
}

function AddressStep({
  address,
  onAddress,
  provider,
  detected,
  onProvider,
  wantMail,
  wantCalendar,
  calendarPossible,
  onWant
}: {
  address: string;
  onAddress: (a: string) => void;
  provider: ProviderId | null;
  detected: ProviderId | null;
  onProvider: (p: ProviderId) => void;
  wantMail: boolean;
  wantCalendar: boolean;
  calendarPossible: boolean;
  onWant: (mail: boolean, calendar: boolean) => void;
}) {
  const { t } = useTranslation();
  const own = detected === "other";
  return (
    <>
      <Field label={t(`${K}.address.label`)} htmlFor="connect-mail-address" hint={t(`${K}.address.hint`)}>
        <input
          id="connect-mail-address"
          className={formStyles.input}
          type="email"
          value={address}
          autoComplete="email"
          spellCheck={false}
          placeholder="you@example.com"
          data-testid="connect-mail-address"
          onChange={(e) => onAddress(e.target.value)}
        />
      </Field>

      {provider ? (
        <div className={styles.section} data-testid="connect-mail-provider">
          <p className={styles.text} role="status">
            {own
              ? t(`${K}.address.unknownDomain`, { domain: domainOf(address) })
              : t(`${K}.address.detected`, { provider: t(`${K}.providers.${provider}`) })}
          </p>
          <Field label={t(`${K}.address.providerLabel`)} htmlFor="connect-mail-provider-select">
            <select
              id="connect-mail-provider-select"
              className={formStyles.select}
              value={provider}
              data-testid="connect-mail-provider-select"
              onChange={(e) => onProvider(e.target.value as ProviderId)}
            >
              {PROVIDER_IDS.map((id) => (
                <option key={id} value={id}>
                  {t(`${K}.providers.${id}`)}
                </option>
              ))}
            </select>
          </Field>
        </div>
      ) : null}

      {provider ? (
        <div className={styles.section}>
          <h3 className={styles.sectionTitle}>{t(`${K}.want.title`)}</h3>
          <div className={styles.choices}>
            <label className={styles.choice}>
              <input
                type="checkbox"
                checked={wantMail}
                data-testid="connect-mail-want-mail"
                onChange={(e) => onWant(e.target.checked, wantCalendar)}
              />
              <span>
                <Mail aria-hidden="true" /> {t(`${K}.want.mail`)}
              </span>
            </label>
            <label className={styles.choice}>
              <input
                type="checkbox"
                checked={wantCalendar && calendarPossible}
                disabled={!calendarPossible}
                data-testid="connect-mail-want-calendar"
                onChange={(e) => onWant(wantMail, e.target.checked)}
              />
              <span>
                <CalendarDays aria-hidden="true" /> {t(`${K}.want.calendar`)}
                {!calendarPossible ? (
                  <span className={styles.choiceHint}>{t(`${K}.calendarNone.${provider}`)}</span>
                ) : null}
              </span>
            </label>
          </div>
        </div>
      ) : null}
    </>
  );
}

/** The sign-in paths that happen in the Apps window: Google's own servers, Outlook in the hub. */
function SignInBlock({
  kind,
  done,
  checked,
  checking,
  onOpen,
  onCheck
}: {
  kind: "google" | "microsoft";
  done: boolean;
  checked: boolean;
  checking: boolean;
  onOpen: () => void;
  onCheck: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className={styles.section} data-testid={`connect-mail-signin-${kind}`}>
      <h3 className={styles.sectionTitle}>{t(`${K}.signIn.${kind}.title`)}</h3>
      <p className={styles.text}>{t(`${K}.signIn.${kind}.body`)}</p>
      <div className={styles.row}>
        <Button type="button" size="sm" variant="secondary" data-testid={`connect-mail-open-${kind}`} onClick={onOpen}>
          {t(`${K}.signIn.${kind}.open`)}
        </Button>
        <Button
          type="button"
          size="sm"
          variant="ghost"
          disabled={checking}
          data-testid={`connect-mail-check-${kind}`}
          onClick={onCheck}
        >
          {checking ? t(`${K}.signIn.checking`) : t(`${K}.signIn.check`)}
        </Button>
      </div>
      {checked ? (
        <p
          className={[styles.line, done ? styles.lineOk : styles.lineBad].join(" ")}
          role="status"
          data-testid={`connect-mail-signin-${kind}-status`}
        >
          {done ? <CircleCheck aria-hidden="true" /> : <CircleAlert aria-hidden="true" />}
          <span>{t(`${K}.signIn.${kind}.${done ? "connected" : "notYet"}`)}</span>
        </p>
      ) : null}
    </div>
  );
}

/** What the live test found, in plain words, with the one fix for each thing that did not work. */
function TestResults({
  provider,
  test,
  picked,
  onPick,
  allowSend
}: {
  provider: ProviderId;
  test: GuideTest;
  picked: string[];
  onPick: (urls: string[]) => void;
  allowSend: boolean;
}) {
  const { t } = useTranslation();
  const line = (ok: boolean, body: string, detail?: string | null) => (
    <li className={[styles.line, ok ? styles.lineOk : styles.lineBad].join(" ")}>
      {ok ? <CircleCheck aria-hidden="true" /> : <CircleAlert aria-hidden="true" />}
      <span className={styles.lineText}>
        <span>{body}</span>
        {detail ? <span className={styles.detail}>{detail}</span> : null}
      </span>
    </li>
  );
  const mail = test.mail;
  const calendar = test.calendar;
  return (
    <ul className={styles.result} data-testid="connect-mail-results" aria-live="polite">
      {mail?.state === "error"
        ? line(false, t(`${K}.result.error`, { message: humanLine(mail.error) }))
        : mail
          ? (() => {
              const r = mail.report;
              const failing = r.imap !== "ok" ? r.imap : r.smtp;
              const fix = mailFixKey(provider, failing);
              if (r.ok) {
                return line(true, t(`${K}.result.mailOk`));
              }
              if (mail.state === "ok") {
                // Reading works; only the outgoing server did not. Fine while the bot may not send.
                return [
                  line(true, t(`${K}.result.mailReadOk`), t(`${K}.result.mailReadNote`)),
                  fix ? <li key="smtp-note" className={styles.detail}>{t(`${K}.fix.${fix}`)}</li> : null
                ];
              }
              return line(false, t(`${K}.result.mailBad`), fix ? t(`${K}.fix.${fix}`) : null);
            })()
          : null}
      {calendar?.state === "error"
        ? line(false, t(`${K}.result.error`, { message: humanLine(calendar.error) }))
        : calendar?.state === "ok"
          ? line(true, t(`${K}.result.calendarOk`, { count: calendar.report.calendars.length }))
          : calendar
            ? line(
                false,
                t(`${K}.result.calendarBad`),
                `${t(`${K}.fix.${calendarFixKey(provider, calendar.report.error)}`)}${
                  calendarProblem(calendar.report.error) === "other" && calendar.report.error
                    ? ` ${t(`${K}.fix.calendar.detail`, { error: calendar.report.error })}`
                    : ""
                }`
              )
            : null}
      {calendar?.state === "ok" && calendar.report.calendars.length > 1 ? (
        <li>
          <p className={styles.text}>{t(`${K}.result.pick`)}</p>
          <ul className={styles.found}>
            {calendar.report.calendars.map((c) => (
              <li key={c.url}>
                <label>
                  <input
                    type="checkbox"
                    checked={picked.includes(c.url)}
                    onChange={(e) =>
                      onPick(e.target.checked ? [...picked, c.url] : picked.filter((u) => u !== c.url))
                    }
                  />
                  {c.name}
                </label>
              </li>
            ))}
          </ul>
        </li>
      ) : null}
      {mail?.state === "ok" ? (
        <li className={styles.detail}>{allowSend ? t(`${K}.result.sendAsks`) : t(`${K}.result.readOnlyOn`)}</li>
      ) : null}
    </ul>
  );
}

function Done({
  provider,
  saved,
  allowSend,
  botName,
  signedIn,
  toolsOn,
  onTurnOn,
  accessUrl,
  problem
}: {
  provider: ProviderId | null;
  saved: GuideSaved | null;
  allowSend: boolean;
  botName: string;
  signedIn: { google?: boolean; microsoft?: boolean };
  toolsOn: boolean | null;
  onTurnOn: () => void;
  accessUrl: string | null;
  problem: string | null;
}) {
  const { t } = useTranslation();
  const providerName = provider ? t(`${K}.providers.${provider}`) : "";
  const lines: string[] = [];
  if (saved?.mail) lines.push(t(`${K}.done.mail`, { name: saved.mail.name }));
  if (saved && saved.calendars.length > 0) lines.push(t(`${K}.done.calendar`, { count: saved.calendars.length }));
  if (signedIn.google) lines.push(t(`${K}.signIn.google.connected`));
  if (signedIn.microsoft) lines.push(t(`${K}.signIn.microsoft.connected`));
  return (
    <div className={styles.section} data-testid="connect-mail-done">
      <Callout tone="success" title={t(`${K}.done.title`)}>
        {lines.length > 0 ? (
          <ul className={styles.result}>
            {lines.map((l) => (
              <li key={l}>{l}</li>
            ))}
          </ul>
        ) : (
          t(`${K}.done.nothing`)
        )}
      </Callout>
      {saved?.mail ? (
        <p className={styles.text} data-testid="connect-mail-done-send">
          {allowSend ? t(`${K}.done.sendAsks`) : t(`${K}.done.readOnly`)}
        </p>
      ) : null}
      {saved?.mail && toolsOn === false ? (
        <div className={styles.row}>
          <Callout tone="warning">{t(`${K}.done.toolsOff`, { name: botName })}</Callout>
          <Button size="sm" variant="secondary" data-testid="connect-mail-tools-on" onClick={onTurnOn}>
            {t(`${K}.done.turnOn`, { name: botName })}
          </Button>
        </div>
      ) : null}
      {saved && saved.failed.length > 0 ? (
        <Callout tone="danger" role="alert">
          {t(`${K}.done.partFailed`, { message: problem ?? humanLine(saved.failed[0].error) })}
        </Callout>
      ) : null}
      <p className={styles.text}>{t(`${K}.done.revoke`)}</p>
      {accessUrl ? <Link href={accessUrl}>{t(`${K}.done.access`, { provider: providerName })}</Link> : null}
    </div>
  );
}

/** The flow in its dialog. */
export function ConnectMailModal({
  open,
  client,
  bots,
  onClose,
  onChanged
}: {
  open: boolean;
  client: FlowClient | null;
  bots: Bot[];
  onClose: () => void;
  onChanged?: () => void;
}) {
  const { t } = useTranslation();
  return (
    <Modal open={open} onClose={onClose} size="md" label={t(`${K}.title`)} describedBy={SUBTITLE_ID}>
      {open ? <ConnectMailFlow client={client} bots={bots} onClose={onClose} onChanged={onChanged} /> : null}
    </Modal>
  );
}
