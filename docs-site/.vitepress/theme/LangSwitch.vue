<script setup lang="ts">
import { computed, watchEffect } from "vue";
import { useData, withBase } from "vitepress";
import { LANGS, currentLang, switchTarget } from "./langs";

const { page, theme } = useData();
const lang = computed(() => currentLang(page.value.relativePath));
const links = computed(() =>
  LANGS.map((l) => {
    const path = switchTarget(page.value.relativePath, l.code, theme.value.translated ?? {});
    return { ...l, href: withBase("/" + path.replace(/(^|\/)index$/, "$1")) };
  }),
);

// The site shell is English; the page content language is what screen readers need.
watchEffect(() => {
  if (typeof document !== "undefined") document.documentElement.lang = lang.value;
});
</script>

<template>
  <nav class="praxi-lang" aria-label="Language">
    <template v-for="l in links" :key="l.code">
      <span v-if="l.code === lang" aria-current="true" :lang="l.code">{{ l.label }}</span>
      <a v-else :href="l.href" :hreflang="l.code" :lang="l.code">{{ l.label }}</a>
    </template>
  </nav>
</template>
