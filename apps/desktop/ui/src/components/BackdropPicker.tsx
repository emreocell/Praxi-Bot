import { Check, Ban, ImageIcon, Plus } from "lucide-react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import {
  BACKDROP_DIM_MAX,
  BACKDROP_DIM_MIN,
  backdropsInGroup,
  CUSTOM_BACKDROP,
  dimToSlider,
  FAVORITE_BACKDROPS,
  NO_BACKDROP,
  sliderToDim,
  type BackdropGroup
} from "../lib/backdropCatalog";
import type { SceneColors } from "../lib/backdropDraw";
import { BackdropThumb } from "./Backdrop";
import { Modal } from "./Modal";
import styles from "./AppearancePicker.module.css";

const GROUPS: readonly BackdropGroup[] = ["dotted", "grain", "glass"];

export interface BackdropPatch {
  backdrop?: string;
  backdropDim?: number;
  backgroundUrl?: string;
  backgroundFileName?: string;
}

export function BackdropPicker({
  backdrop,
  dim,
  backgroundUrl,
  colors,
  onChange
}: {
  backdrop: string;
  dim: number;
  backgroundUrl: string;
  colors: SceneColors;
  onChange: (patch: BackdropPatch) => void;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const fileRef = useRef<HTMLInputElement | null>(null);
  const nameOf = (id: string) => t(`backdrop.names.${id}`);
  const isFavorite = backdrop === NO_BACKDROP || FAVORITE_BACKDROPS.includes(backdrop);

  const onFile = (file: File | undefined) => {
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      const result = typeof reader.result === "string" ? reader.result : "";
      if (result) onChange({ backgroundUrl: result, backgroundFileName: file.name, backdrop: CUSTOM_BACKDROP });
    };
    reader.readAsDataURL(file);
  };

  const sceneTile = (id: string) => (
    <button
      key={id}
      type="button"
      className={styles.tile}
      aria-pressed={backdrop === id}
      onClick={() => onChange({ backdrop: id })}
    >
      <span className={styles.preview}>
        <BackdropThumb scene={id} colors={colors} />
      </span>
      <span className={styles.label}>
        {nameOf(id)}
        {backdrop === id ? <Check /> : null}
      </span>
    </button>
  );
  const noneTile = (
    <button
      type="button"
      className={styles.tile}
      aria-pressed={backdrop === NO_BACKDROP}
      onClick={() => onChange({ backdrop: NO_BACKDROP })}
    >
      <span className={styles.preview}>
        <Ban />
      </span>
      <span className={styles.label}>
        {t("appearance.backdrop.none")}
        {backdrop === NO_BACKDROP ? <Check /> : null}
      </span>
    </button>
  );
  const slider = backdrop !== NO_BACKDROP ? (
    <label className={styles.range}>
      <span>{t("appearance.backdrop.calm")}</span>
      <input
        type="range"
        min={BACKDROP_DIM_MIN}
        max={BACKDROP_DIM_MAX}
        value={dimToSlider(dim)}
        aria-label={t("appearance.backdrop.intensity")}
        onChange={(e) => onChange({ backdropDim: sliderToDim(Number(e.target.value)) })}
      />
      <span>{t("appearance.backdrop.lively")}</span>
    </label>
  ) : null;

  return (
    <div className={styles.root}>
      <div className={styles.tiles}>
        {noneTile}
        {FAVORITE_BACKDROPS.map(sceneTile)}
        <button
          type="button"
          className={styles.tile}
          aria-pressed={!isFavorite}
          aria-haspopup="dialog"
          onClick={() => setOpen(true)}
        >
          <span className={`${styles.preview} ${styles.more}`}>
            {isFavorite ? <Plus /> : backdrop === CUSTOM_BACKDROP ? <ImageIcon /> : <BackdropThumb scene={backdrop} colors={colors} />}
          </span>
          <span className={styles.label}>
            {isFavorite ? t("appearance.backdrop.all") : nameOf(backdrop)}
          </span>
        </button>
      </div>
      {slider}

      <Modal open={open} onClose={() => setOpen(false)} label={t("appearance.backdrop.modalTitle")} size="md">
        <div className={styles.gallery}>
          <div className={styles.galleryHead}>
            <h3>{t("appearance.backdrop.modalTitle")}</h3>
            <Button size="sm" variant="primary" onClick={() => setOpen(false)} data-autofocus>
              {t("appearance.themes.done")}
            </Button>
          </div>
          <div className={styles.galleryBody}>
            {GROUPS.map((g) => (
              <section key={g} className={styles.gallerySection}>
                <h4>{t(`appearance.backdrop.groups.${g}`)}</h4>
                <div className={styles.tiles}>{backdropsInGroup(g).map((s) => sceneTile(s.id))}</div>
              </section>
            ))}
            <section className={styles.gallerySection}>
              <h4>{t("appearance.backdrop.groups.other")}</h4>
              <input
                ref={fileRef}
                type="file"
                hidden
                accept="image/png,image/jpeg,image/webp,image/gif,image/svg+xml"
                onChange={(e) => {
                  onFile(e.target.files?.[0]);
                  e.target.value = "";
                }}
              />
              <div className={styles.tiles}>
                {noneTile}
                <button
                  type="button"
                  className={styles.tile}
                  aria-pressed={backdrop === CUSTOM_BACKDROP}
                  onClick={() => (backgroundUrl ? onChange({ backdrop: CUSTOM_BACKDROP }) : fileRef.current?.click())}
                >
                  <span className={`${styles.preview} ${styles.more}`}>
                    {backgroundUrl ? <img src={backgroundUrl} alt="" className={styles.cover} /> : <ImageIcon />}
                  </span>
                  <span className={styles.label}>
                    {t("appearance.backdrop.own")}
                    {backdrop === CUSTOM_BACKDROP ? <Check /> : null}
                  </span>
                </button>
              </div>
              <p className={styles.hint}>{t("appearance.backdrop.ownHint")}</p>
              <div className={styles.row}>
                <Button size="sm" variant="secondary" onClick={() => fileRef.current?.click()}>
                  {t("appearance.backdrop.ownChoose")}
                </Button>
              </div>
              {slider}
            </section>
          </div>
        </div>
      </Modal>
    </div>
  );
}
