<script lang="ts" module>
  // 24×24 stroke icons. Directional ones mirror in right-to-left layouts.
  const PATHS = {
    devices: 'M4 5h16v10H4zM2 19h20M9 15v4M15 15v4',
    transfers: 'M7 7h13l-4-4M17 17H4l4 4',
    history: 'M3 12a9 9 0 1 0 3-6.7L3 8M3 3v5h5M12 7v5l3 3',
    settings:
      'M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z',
    send: 'M5 12h14M13 6l6 6-6 6',
    receive: 'M19 12H5M11 6l-6 6 6 6',
    'chevron-start': 'M15 6l-6 6 6 6',
    'chevron-end': 'M9 6l6 6-6 6',
    pause: 'M8 5v14M16 5v14',
    play: 'M7 5l12 7-12 7z',
    close: 'M6 6l12 12M18 6L6 18',
    check: 'M5 12l5 5 9-10',
    folder: 'M3 6h6l2 2h10v11H3z',
    shield: 'M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z',
    upload: 'M12 16V4M7 9l5-5 5 5M4 20h16',
    trash: 'M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3',
    windows: 'M3 5l8-1v8H3zM13 4l8-1v9h-8zM3 13h8v8l-8-1zM13 13h8v9l-8-1z',
    macos: 'M4 4h16v12H4zM8 20h8M12 16v4',
    linux: 'M5 4h14v12H5zM3 20h18M9 9l2 2-2 2M13 13h3',
    alert: 'M12 8v5M12 16v.5M10.3 3.9L2 18a2 2 0 0 0 1.7 3h16.6a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z',
  } as const;

  export type IconName = keyof typeof PATHS;

  const DIRECTIONAL: ReadonlySet<IconName> = new Set(['send', 'receive', 'chevron-start', 'chevron-end']);
</script>

<script lang="ts">
  interface Props {
    name: IconName;
    size?: number;
  }

  let { name, size = 20 }: Props = $props();
</script>

<svg
  class="icon"
  class:icon-directional={DIRECTIONAL.has(name)}
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="2"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  <path d={PATHS[name]} />
</svg>

<style>
  .icon {
    flex-shrink: 0;
  }

  :global(:dir(rtl)) .icon-directional {
    transform: scaleX(-1);
  }
</style>
