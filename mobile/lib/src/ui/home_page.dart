import 'dart:io';

import 'package:flutter/material.dart';

import '../engine/engine.dart';
import '../platform.dart';
import '../queue.dart';
import '../settings.dart';
import 'format.dart';
import 'photo_page.dart';
import 'settings_page.dart';

class HomePage extends StatelessWidget {
  const HomePage({
    super.key,
    required this.settings,
    required this.queue,
    this.engine = const Engine(),
    this.platform = const PlatformServices(),
  });

  final SettingsController settings;
  final QueueController queue;
  final Engine engine;
  final PlatformServices platform;

  Future<void> _add(BuildContext context, Future<List<PhotoItem>> Function() pick) async {
    try {
      final items = await pick();
      queue.add(items);
    } catch (e) {
      if (context.mounted) showError(context, 'Cannot open the photos: $e');
    }
  }

  void _openSettings(BuildContext context) => Navigator.of(context).push(
    MaterialPageRoute<void>(
      builder: (_) => SettingsPage(settings: settings, engine: engine),
    ),
  );

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: Listenable.merge([settings, queue]),
      builder: (context, _) {
        final items = queue.items;
        return Scaffold(
          appBar: AppBar(
            title: const Text('MediaAnonymizer'),
            actions: [
              if (queue.doneCount > 0 && !queue.isRunning)
                PopupMenuButton<String>(
                  tooltip: 'More',
                  onSelected: (v) {
                    if (v == 'clear') queue.clearFinished();
                  },
                  itemBuilder: (_) => const [PopupMenuItem(value: 'clear', child: Text('Clear finished'))],
                ),
              IconButton(tooltip: 'Settings', icon: const Icon(Icons.tune), onPressed: () => _openSettings(context)),
            ],
          ),
          body: items.isEmpty
              ? _EmptyState(
                  onPick: () => _add(context, platform.pickPhotos),
                  onCamera: () => _add(context, () async => [?await platform.takePhoto()]),
                )
              : ListView.separated(
                  padding: const EdgeInsets.only(bottom: 24),
                  itemCount: items.length,
                  separatorBuilder: (_, _) => const Divider(height: 1, indent: 88),
                  itemBuilder: (context, i) => _PhotoTile(
                    item: items[i],
                    queue: queue,
                    platform: platform,
                    onTap: () => Navigator.of(context).push(
                      MaterialPageRoute<void>(
                        builder: (_) => PhotoPage(
                          item: items[i],
                          queue: queue,
                          settings: settings,
                          engine: engine,
                          platform: platform,
                        ),
                      ),
                    ),
                  ),
                ),
          bottomNavigationBar: items.isEmpty
              ? null
              : _ActionBar(
                  settings: settings.value,
                  queue: queue,
                  onAdd: () => _add(context, platform.pickPhotos),
                  onCamera: () => _add(context, () async => [?await platform.takePhoto()]),
                  onSettings: () => _openSettings(context),
                ),
        );
      },
    );
  }
}

class _EmptyState extends StatelessWidget {
  const _EmptyState({required this.onPick, required this.onCamera});

  final VoidCallback onPick;
  final VoidCallback onCamera;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Center(
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(32),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.shield_outlined, size: 72, color: theme.colorScheme.primary),
            const SizedBox(height: 16),
            Text('Anonymize your photos', style: theme.textTheme.headlineSmall, textAlign: TextAlign.center),
            const SizedBox(height: 8),
            Text(
              'Remove location and personal metadata, and hide faces — '
              'without losing quality. Everything stays on this device.',
              style: theme.textTheme.bodyMedium?.copyWith(color: theme.colorScheme.onSurfaceVariant),
              textAlign: TextAlign.center,
            ),
            const SizedBox(height: 24),
            FilledButton.icon(
              onPressed: onPick,
              icon: const Icon(Icons.add_photo_alternate_outlined),
              label: const Text('Choose photos'),
            ),
            const SizedBox(height: 8),
            OutlinedButton.icon(
              onPressed: onCamera,
              icon: const Icon(Icons.photo_camera_outlined),
              label: const Text('Take a photo'),
            ),
          ],
        ),
      ),
    );
  }
}

class _PhotoTile extends StatelessWidget {
  const _PhotoTile({required this.item, required this.queue, required this.platform, required this.onTap});

  final PhotoItem item;
  final QueueController queue;
  final PlatformServices platform;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final report = item.report;
    final Widget subtitle = switch (item.status) {
      ItemStatus.pending => const Text('Ready'),
      ItemStatus.processing => const Padding(
        padding: EdgeInsets.only(top: 8, right: 16),
        child: LinearProgressIndicator(),
      ),
      ItemStatus.done => Text(
        [
          summarizeReport(report!),
          if (item.savedToGallery) 'saved to Photos',
          if (item.galleryError != null) 'not saved to Photos',
        ].join(' · '),
      ),
      ItemStatus.failed => Text(
        item.error ?? 'Failed',
        maxLines: 2,
        overflow: TextOverflow.ellipsis,
        style: TextStyle(color: theme.colorScheme.error),
      ),
      ItemStatus.cancelled => const Text('Cancelled'),
    };
    final Widget? trailing = switch (item.status) {
      ItemStatus.done => Builder(
        builder: (context) => IconButton(
          tooltip: 'Share',
          icon: const Icon(Icons.ios_share),
          onPressed: () => platform.share(context, [report!.output]),
        ),
      ),
      ItemStatus.failed || ItemStatus.cancelled when !queue.isRunning => IconButton(
        tooltip: 'Retry',
        icon: const Icon(Icons.refresh),
        onPressed: () => queue.retry(item),
      ),
      ItemStatus.processing => null,
      _ => IconButton(
        tooltip: 'Remove',
        icon: const Icon(Icons.close),
        onPressed: queue.isRunning ? null : () => queue.remove(item),
      ),
    };
    return ListTile(
      contentPadding: const EdgeInsets.symmetric(horizontal: 16, vertical: 4),
      leading: ClipRRect(
        borderRadius: BorderRadius.circular(8),
        child: SizedBox.square(
          dimension: 56,
          child: Image.file(
            File(report?.output ?? item.input),
            fit: BoxFit.cover,
            cacheWidth: 168,
            errorBuilder: (_, _, _) => ColoredBox(
              color: theme.colorScheme.surfaceContainerHighest,
              child: const Icon(Icons.image_not_supported_outlined),
            ),
          ),
        ),
      ),
      title: Row(
        children: [
          Flexible(child: Text(item.name, maxLines: 1, overflow: TextOverflow.ellipsis)),
          if (item.status == ItemStatus.done) ...[
            const SizedBox(width: 6),
            Icon(Icons.check_circle, size: 18, color: theme.colorScheme.primary),
          ],
        ],
      ),
      subtitle: subtitle,
      trailing: trailing,
      onTap: onTap,
    );
  }
}

class _ActionBar extends StatelessWidget {
  const _ActionBar({
    required this.settings,
    required this.queue,
    required this.onAdd,
    required this.onCamera,
    required this.onSettings,
  });

  final Settings settings;
  final QueueController queue;
  final VoidCallback onAdd;
  final VoidCallback onCamera;
  final VoidCallback onSettings;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final problem = settings.problem;
    final pending = queue.pendingCount;
    final (finished, total) = queue.progress;
    return Material(
      color: theme.colorScheme.surfaceContainer,
      child: SafeArea(
        top: false,
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 12, 16, 12),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              InkWell(
                onTap: queue.isRunning ? null : onSettings,
                borderRadius: BorderRadius.circular(8),
                child: Padding(
                  padding: const EdgeInsets.symmetric(vertical: 4),
                  child: Text(
                    problem ?? describeSettings(settings),
                    style: theme.textTheme.bodySmall?.copyWith(
                      color: problem != null ? theme.colorScheme.error : theme.colorScheme.onSurfaceVariant,
                    ),
                  ),
                ),
              ),
              const SizedBox(height: 8),
              if (queue.isRunning) ...[
                LinearProgressIndicator(value: total == 0 ? null : finished / total),
                const SizedBox(height: 8),
                Row(
                  children: [
                    Expanded(child: Text('Processing ${finished + 1} of $total…')),
                    TextButton(onPressed: queue.cancel, child: const Text('Cancel')),
                  ],
                ),
              ] else
                Row(
                  children: [
                    IconButton.outlined(tooltip: 'Add photos', onPressed: onAdd, icon: const Icon(Icons.add)),
                    const SizedBox(width: 8),
                    IconButton.outlined(
                      tooltip: 'Take a photo',
                      onPressed: onCamera,
                      icon: const Icon(Icons.photo_camera_outlined),
                    ),
                    const SizedBox(width: 12),
                    Expanded(
                      child: pending == 0 && queue.failedCount > 0
                          ? FilledButton.tonalIcon(
                              onPressed: () => queue.retry(),
                              icon: const Icon(Icons.refresh),
                              label: Text('Retry ${queue.failedCount}'),
                            )
                          : FilledButton.icon(
                              onPressed: pending == 0 || problem != null ? null : () => queue.start(settings),
                              icon: const Icon(Icons.shield_outlined),
                              label: Text(pending == 0 ? 'All done' : 'Anonymize ${plural(pending, 'photo')}'),
                            ),
                    ),
                  ],
                ),
            ],
          ),
        ),
      ),
    );
  }
}
