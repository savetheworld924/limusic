<script lang="ts">
	// Psychoacoustic enhancer (mpv `af` chain, default OFF). Settings live in
	// Rust via `set_setting` like normalize_volume/crossfade: enabled, preset,
	// amounts JSON blob, output, bypass. The backend applies them to what is
	// already playing, so every control here is audible immediately.
	import { Button } from '$lib/components/ui/button';
	import { Slider } from '$lib/components/ui/slider';
	import { Switch } from '$lib/components/ui/switch';
	import * as Select from '$lib/components/ui/select';
	import * as api from '$lib/api';

	let { settings }: { settings: Record<string, string> } = $props();

	const PRESETS = ['Subtle', 'Warm', 'Wide', 'Reference'] as const;

	type StageKey = 'loudness' | 'polish' | 'exciter' | 'virtual_bass' | 'stereo' | 'room';
	type Amounts = Record<StageKey, number>;

	const STAGES: { key: StageKey; label: string; hint: string }[] = [
		{ key: 'loudness', label: 'Loudness', hint: 'Bass + treble lift as volume drops' },
		{ key: 'polish', label: 'Polish', hint: 'De-mud, presence, air (±2 dB max)' },
		{ key: 'exciter', label: 'Exciter', hint: 'High shimmer above ~6 kHz' },
		{ key: 'virtual_bass', label: 'Virtual bass', hint: 'Implied low end for small speakers' },
		{ key: 'stereo', label: 'Stereo', hint: 'Width (speakers) / crossfeed (headphones)' },
		{ key: 'room', label: 'Room', hint: 'Faint early reflections (off in Subtle)' }
	];

	function presetAmounts(p: string): Amounts {
		switch (p) {
			case 'Warm':
				return { loudness: 0.7, polish: 0.3, exciter: 0.15, virtual_bass: 0.6, stereo: 0.3, room: 0.15 };
			case 'Wide':
				return { loudness: 0.5, polish: 0.5, exciter: 0.35, virtual_bass: 0.25, stereo: 0.8, room: 0.1 };
			case 'Reference':
				return { loudness: 0.5, polish: 0, exciter: 0, virtual_bass: 0, stereo: 0, room: 0 };
			default:
				return { loudness: 0.6, polish: 0.4, exciter: 0.25, virtual_bass: 0.3, stereo: 0.4, room: 0 };
		}
	}

	function readAmounts(): Amounts {
		const fallback = presetAmounts(settings.enhancer_preset ?? 'Subtle');
		try {
			const raw = settings.enhancer_amounts;
			if (!raw) return fallback;
			const v = JSON.parse(raw) as Partial<Record<StageKey, unknown>>;
			const num = (k: StageKey) => {
				const n = Number(v[k]);
				return Number.isFinite(n) ? Math.min(1, Math.max(0, n)) : fallback[k];
			};
			return {
				loudness: num('loudness'),
				polish: num('polish'),
				exciter: num('exciter'),
				virtual_bass: num('virtual_bass'),
				stereo: num('stereo'),
				room: num('room')
			};
		} catch {
			return fallback;
		}
	}

	const enabled = $derived(settings.enhancer_enabled === 'true');
	const bypass = $derived(settings.enhancer_bypass === 'true');
	const preset = $derived(settings.enhancer_preset ?? 'Subtle');
	const output = $derived(settings.enhancer_output ?? 'speakers');
	const amounts = $derived(readAmounts());
	const pct = (v: number) => `${Math.round(v * 100)}%`;

	async function setEnabled(on: boolean) {
		settings.enhancer_enabled = on ? 'true' : 'false';
		await api.setSetting('enhancer_enabled', settings.enhancer_enabled);
	}

	async function setPreset(p: string) {
		const a = presetAmounts(p);
		settings.enhancer_preset = p;
		settings.enhancer_amounts = JSON.stringify(a);
		await api.setSetting('enhancer_preset', p);
		await api.setSetting('enhancer_amounts', settings.enhancer_amounts);
	}

	async function setOutput(o: string) {
		settings.enhancer_output = o;
		await api.setSetting('enhancer_output', o);
	}

	async function setBypass(b: boolean) {
		settings.enhancer_bypass = b ? 'true' : 'false';
		await api.setSetting('enhancer_bypass', settings.enhancer_bypass);
	}

	async function setStage(key: StageKey, v: number) {
		const a = { ...amounts, [key]: Math.min(1, Math.max(0, v / 100)) };
		settings.enhancer_amounts = JSON.stringify(a);
		await api.setSetting('enhancer_amounts', settings.enhancer_amounts);
	}
</script>

<div class="overflow-hidden rounded-xl border bg-card">
	<div class="px-4 py-3.5">
		<div class="flex items-center justify-between gap-6">
			<div class="min-w-0">
				<p class="text-sm font-medium">Psychoacoustic enhancer</p>
				<p class="mt-0.5 text-xs leading-relaxed text-muted-foreground">
					Subtle detail, width and weight. Off by default; level-matched so A/B compares
					fairly.
				</p>
			</div>
			<Switch checked={enabled} onCheckedChange={setEnabled} />
		</div>
	</div>
	{#if enabled}
		<div class="border-t px-4 py-3.5">
			<div class="flex items-center justify-between gap-6">
				<div class="min-w-0">
					<p class="text-sm font-medium">Preset</p>
					<p class="mt-0.5 text-xs leading-relaxed text-muted-foreground">
						Subtle is the default. Reference is almost transparent.
					</p>
				</div>
				<Select.Root type="single" value={preset} onValueChange={(v) => setPreset(v)}>
					<Select.Trigger class="w-44 shrink-0">
						<span class="flex-1 truncate text-left">{preset}</span>
					</Select.Trigger>
					<Select.Content>
						{#each PRESETS as p (p)}
							<Select.Item value={p} label={p}>{p}</Select.Item>
						{/each}
					</Select.Content>
				</Select.Root>
			</div>
		</div>
		<div class="border-t px-4 py-3.5">
			<div class="flex items-center justify-between gap-6">
				<div class="min-w-0">
					<p class="text-sm font-medium">Listening on</p>
					<p class="mt-0.5 text-xs leading-relaxed text-muted-foreground">
						Headphones adds mild crossfeed; speakers widens the sides only.
					</p>
				</div>
				<div class="flex shrink-0 rounded-lg bg-muted p-0.5">
					{#each ['speakers', 'headphones'] as o (o)}
						<button
							type="button"
							onclick={() => setOutput(o)}
							aria-pressed={output === o}
							class="cursor-pointer rounded-md px-3.5 py-1.5 text-xs font-medium transition-colors {output ===
							o
								? 'bg-background text-foreground shadow-sm'
								: 'text-muted-foreground hover:text-foreground'}"
						>
							{o === 'speakers' ? 'Speakers' : 'Headphones'}
						</button>
					{/each}
				</div>
			</div>
		</div>
		{#each STAGES as s (s.key)}
			<div class="border-t px-4 py-3.5">
				<div class="flex items-center justify-between gap-6">
					<div class="min-w-0">
						<p class="text-sm font-medium">{s.label}</p>
						<p class="mt-0.5 text-xs leading-relaxed text-muted-foreground">{s.hint}</p>
					</div>
					<div class="flex w-44 shrink-0 items-center gap-3">
						<Slider
							type="single"
							aria-label={s.label}
							min={0}
							max={100}
							step={1}
							value={Math.round(amounts[s.key] * 100)}
							onValueChange={(v) => setStage(s.key, v)}
						/>
						<span class="w-10 shrink-0 text-right font-mono text-xs text-muted-foreground">
							{pct(amounts[s.key])}
						</span>
					</div>
				</div>
			</div>
		{/each}
		<div class="border-t px-4 py-3.5">
			<Button
				variant={bypass ? 'secondary' : 'default'}
				size="lg"
				class="w-full cursor-pointer"
				onclick={() => setBypass(!bypass)}
			>
				{bypass ? 'A/B: hearing Original — switch to Enhanced' : 'A/B: hearing Enhanced — switch to Original'}
			</Button>
			<p class="mt-2 text-center text-xs leading-relaxed text-muted-foreground">
				Level-matched within ~0.5 dB, so louder never wins by accident.
			</p>
		</div>
	{/if}
</div>
