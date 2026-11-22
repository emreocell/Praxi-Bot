import { useReducer, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Save, Workflow, X } from "lucide-react";
import { Button } from "ui";
import type { ApiClient } from "../api/client";
import { humanLine } from "../lib/humanError";
import { canOfferWorkflow, EMPTY_REVIEW, reviewReducer } from "../lib/workflowReview";
import { Modal } from "./Modal";
import styles from "./WorkflowReview.module.css";

/** Fetching a proposal never persists it. Only the explicit Save handler writes. */
export function WorkflowReview({ client, runId, canRemember }: { client: ApiClient; runId: string; canRemember: boolean }) {
  const { t } = useTranslation();
  const [state, dispatch] = useReducer(reviewReducer, EMPTY_REVIEW);
  const saving = useRef(false);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const open = async () => {
    if (loading) return;
    if (state.proposal) { dispatch({ type: "open" }); return; }
    setLoading(true);
    setLoadError(null);
    try {
      const proposal = await client.proposeWorkflow(runId);
      if (!canOfferWorkflow(canRemember, proposal.workflow.steps.length)) { setLoadError(t("workflow.tooFewSteps")); return; }
      dispatch({ type: "loaded", proposal });
      dispatch({ type: "open" });
    } catch (error) { setLoadError(humanLine(error)); }
    finally { setLoading(false); }
  };
  if (!canRemember) return null;
  if (!state.proposal || !state.draft) return <>
    <Button size="sm" variant="ghost" disabled={loading} onClick={() => void open()} data-testid="save-workflow"><Workflow />{t("workflow.button")}</Button>
    {loadError && <p role="alert">{loadError}</p>}
  </>;
  const { proposal, draft } = state;
  const busy = state.phase === "saving";
  const editParam = (index: number, value: string | null) => dispatch({
    type: "edit", draft: { ...draft, params: draft.params.map((param, i) => i === index ? { ...param, default: value } : param) }
  });
  const save = async () => {
    if (proposal.step_provenance.some((step, i) => step.needs_confirmation && !draft.confirmed_steps?.includes(i))) return;
    if (saving.current || state.phase !== "ready" || !state.open || !draft.name.trim()) return;
    saving.current = true;
    dispatch({ type: "save" });
    try {
      await client.saveWorkflow(draft);
      dispatch({ type: "saved" });
    } catch (error) {
      dispatch({ type: "failed", error: humanLine(error) });
    } finally { saving.current = false; }
  };
  return <>
    {state.phase === "saved" ? <span role="status">{t("workflow.saved")}</span> :
      <Button size="sm" variant="ghost" onClick={() => dispatch({ type: "open" })} data-testid="save-workflow"><Workflow />{t("workflow.button")}</Button>}
    <Modal open={state.open} onClose={() => dispatch({ type: "close" })} label={t("workflow.title")} size="lg">
      <div className={styles.root}>
        <div className={styles.head}><h2>{t("workflow.title")}</h2><Button size="sm" variant="ghost" disabled={busy} aria-label={t("workflow.cancel")} title={t("workflow.cancel")} onClick={() => dispatch({ type: "close" })}><X /></Button></div>
        <p>{t("workflow.reviewHint")}</p>
        {proposal.step_provenance.some(step => step.needs_confirmation) && <p role="alert">{t("workflow.untrustedWarning")}</p>}
        <label>{t("workflow.name")}<input value={draft.name} disabled={busy} maxLength={64} pattern="[a-z0-9_-]+" onChange={e => dispatch({ type: "edit", draft: { ...draft, name: e.target.value } })} /></label>
        <label>{t("workflow.description")}<textarea value={draft.description} disabled={busy} onChange={e => dispatch({ type: "edit", draft: { ...draft, description: e.target.value } })} /></label>
        <h3>{t("workflow.steps")}</h3>
        <ol className={styles.steps}>{proposal.workflow.steps.map((step, i) => <li key={i}><pre>{JSON.stringify(step, null, 2)}</pre>
          <p>{proposal.step_provenance[i]?.origin === "user_request" ? t("workflow.fromUser") : t("workflow.fromContent")}</p>
          {proposal.step_provenance[i]?.needs_confirmation && <label><input type="checkbox" disabled={busy} checked={draft.confirmed_steps?.includes(i) ?? false} onChange={e => dispatch({type:"edit",draft:{...draft,confirmed_steps:e.target.checked ? [...(draft.confirmed_steps ?? []), i] : draft.confirmed_steps?.filter(index => index !== i)}})} />{t("workflow.confirmInfluencedStep")}</label>}
        </li>)}</ol>
        <h3>{t("workflow.parameters")}</h3>
        {draft.params.length === 0 ? <p>{t("workflow.noParameters")}</p> : draft.params.map((param, i) => {
          const required = proposal.required_params.includes(param.name);
          return <fieldset key={param.name} disabled={busy} className={styles.parameter}>
            <legend>{param.name} ({param.type})</legend><p>{param.description}</p>
            {required ? <span>{t("workflow.sensitive")}</span> : <>
              <label>{t("workflow.defaultValue")}<textarea rows={3} value={param.default ?? ""} disabled={param.default === null || param.default === undefined} onChange={e => editParam(i, e.target.value)} /></label>
              <label><span><input type="checkbox" checked={param.default === null || param.default === undefined} onChange={e => editParam(i, e.target.checked ? null : "")} />{t("workflow.required")}</span></label>
            </>}
          </fieldset>;
        })}
        {proposal.skipped.length > 0 && <><h3>{t("workflow.skipped")}</h3><ul>{proposal.skipped.map((call, i) => <li key={i}>{call.tool}: {call.reason}</li>)}</ul></>}
        {proposal.warnings.length > 0 && <><h3>{t("workflow.warnings")}</h3><ul>{proposal.warnings.map((warning, i) => <li key={i}>{warning}</li>)}</ul></>}
        {state.error && <p role="alert">{state.error}</p>}
        <div className={styles.actions}><Button variant="ghost" disabled={busy} onClick={() => dispatch({ type: "close" })}>{t("workflow.cancel")}</Button><Button variant="primary" disabled={busy || !/^[a-z0-9_-]{1,64}$/.test(draft.name) || proposal.step_provenance.some((step, i) => step.needs_confirmation && !draft.confirmed_steps?.includes(i))} onClick={() => void save()}><Save />{t(busy ? "workflow.saving" : "workflow.save")}</Button></div>
      </div>
    </Modal>
  </>;
}
