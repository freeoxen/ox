<script lang="ts">
  import "../app.css";
  import ThemePicker from "$lib/components/ThemePicker.svelte";

  let { children } = $props();
</script>

<ThemePicker />
{@render children()}
