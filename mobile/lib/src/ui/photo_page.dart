import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:path/path.dart' as p;
import 'package:path_provider/path_provider.dart';

import '../engine/engine.dart';
import '../platform.dart';
import '../queue.dart';
import '../settings.dart';
import 'format.dart';

enum _View { original, preview, result }

/// One photo: original / redacted preview / result, its metadata and the report.
class PhotoPage extends StatefulWidget {
  const PhotoPage({
    super.key,
    required this.item,
    required this.queue,
    required this.settings,
    this.engine = const Engine(),
    this.platform = const PlatformServices(),
  });

  final PhotoItem item;
  final QueueController queue;
  final SettingsController settings;
  final Engine engine;
  final PlatformServices platform;

  @override
  State<PhotoPage> createState() => _PhotoPageState();
}

class _PhotoPageState extends State<PhotoPage> {
  late final Future<Inspection> _inspection = widget.engine.inspect(widget.item.input);
  Future<Preview>? _preview;
  final List<String> _previewFiles = [];
  late Settings _previewSettings = widget.settings.value;
  late _View _view;
  Timer? _debounce;

  @override
  void initState() {
    super.initState();
    _view = widget.item.status == ItemStatus.done ? _View.result : _View.original;
    _renderPreview();
    if (_preview != null && _view == _View.original) _view = _View.preview;
    widget.settings.addListener(_settingsChanged);
  }

  @override
  void dispose() {
    _debounce?.cancel();
    widget.settings.removeListener(_settingsChanged);
    for (final f in _previewFiles) {
      File(f).delete().ignore();
    }
    super.dispose();
  }

  void _settingsChanged() {
    _debounce?.cancel();
    _debounce = Timer(const Duration(milliseconds: 400), () {
      if (!mounted || widget.settings.value == _previewSettings) return;
      setState(_renderPreview);
    });
  }

  void _renderPreview() {
    final settings = _previewSettings = widget.settings.value;
    if (!settings.changesPixels || settings.problem != null) {
      _preview = null;
      if (_view == _View.preview) _view = _View.original;
      return;
    }
    _preview = () async {
      final dir = await getTemporaryDirectory();
      final out = p.join(dir.path, 'preview-${DateTime.now().microsecondsSinceEpoch}.jpg');
      _previewFiles.add(out);
      return widget.engine.preview(settings.toOptionsJson(), widget.item.input, out);
    }();
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: widget.queue,
      builder: (context, _) {
        final item = widget.item;
        final report = item.report;
        if (report == null && _view == _View.result) _view = _View.original;
        return Scaffold(
          appBar: AppBar(
            title: Text(item.name, overflow: TextOverflow.ellipsis),
            actions: [
              if (report != null && !item.savedToGallery)
                IconButton(
                  tooltip: 'Save to Photos',
                  icon: const Icon(Icons.save_alt),
                  onPressed: () => widget.queue.saveItem(item),
                ),
              if (report != null)
                Builder(
                  builder: (context) => IconButton(
                    tooltip: 'Share',
                    icon: const Icon(Icons.ios_share),
                    onPressed: () => widget.platform.share(context, [report.output]),
                  ),
                ),
            ],
          ),
          body: ListView(
            padding: const EdgeInsets.only(bottom: 32),
            children: [
              _viewSelector(report != null),
              _image(context, report),
              if (report != null) _ReportCard(item: item, report: report),
              if (item.error != null) _ErrorCard(message: item.error!),
              _MetadataCard(inspection: _inspection),
            ],
          ),
        );
      },
    );
  }

  Widget _viewSelector(bool done) {
    final segments = [
      const ButtonSegment(value: _View.original, label: Text('Original')),
      if (_preview != null) const ButtonSegment(value: _View.preview, label: Text('Preview')),
      if (done) const ButtonSegment(value: _View.result, label: Text('Result')),
    ];
    if (segments.length < 2) return const SizedBox(height: 8);
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 8),
      child: SegmentedButton<_View>(
        segments: segments,
        selected: {_view},
        showSelectedIcon: false,
        onSelectionChanged: (s) => setState(() => _view = s.first),
      ),
    );
  }

  Widget _image(BuildContext context, Report? report) {
    final theme = Theme.of(context);
    Widget frame(Widget child, {String? caption}) => Column(
      children: [
        Container(
          constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(context).height * 0.55),
          color: theme.colorScheme.surfaceContainerHighest,
          alignment: Alignment.center,
          child: InteractiveViewer(maxScale: 6, child: child),
        ),
        if (caption != null)
          Padding(
            padding: const EdgeInsets.only(top: 8),
            child: Text(caption, style: theme.textTheme.bodySmall),
          ),
      ],
    );
    final width = (MediaQuery.sizeOf(context).width * MediaQuery.devicePixelRatioOf(context)).round();
    switch (_view) {
      case _View.original:
        // Image.file honours EXIF orientation like the preview does.
        return frame(Image.file(File(widget.item.input), cacheWidth: width, errorBuilder: _broken));
      case _View.result:
        return frame(Image.file(File(report!.output), cacheWidth: width, errorBuilder: _broken));
      case _View.preview:
        return FutureBuilder<Preview>(
          future: _preview,
          builder: (context, snap) {
            if (snap.hasError) {
              return frame(
                Padding(
                  padding: const EdgeInsets.all(24),
                  child: Text('Cannot render the preview: ${snap.error}', textAlign: TextAlign.center),
                ),
              );
            }
            final preview = snap.data;
            if (preview == null) {
              return frame(const Padding(padding: EdgeInsets.all(48), child: CircularProgressIndicator()));
            }
            return frame(
              Image.file(File(preview.path), gaplessPlayback: true, errorBuilder: _broken),
              caption: widget.settings.value.faces ? '${plural(preview.faces, 'face')} found' : null,
            );
          },
        );
    }
  }

  static Widget _broken(BuildContext context, Object error, StackTrace? stack) =>
      const Padding(padding: EdgeInsets.all(48), child: Icon(Icons.broken_image_outlined, size: 48));
}

class _ReportCard extends StatelessWidget {
  const _ReportCard({required this.item, required this.report});

  final PhotoItem item;
  final Report report;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Card(
      margin: const EdgeInsets.fromLTRB(16, 16, 16, 0),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('Result', style: theme.textTheme.titleMedium),
            const SizedBox(height: 8),
            _Line(Icons.face_retouching_off, '${plural(report.faces, 'face')} redacted'),
            if (report.timestamp != null) _Line(Icons.schedule, 'Timestamp: ${report.timestamp}'),
            _Line(
              Icons.straighten,
              '${formatBytes(report.inputBytes)} → ${formatBytes(report.outputBytes)} '
              'in ${(report.elapsed.inMilliseconds / 1000).toStringAsFixed(2)} s',
            ),
            if (item.savedToGallery) const _Line(Icons.photo_library_outlined, 'Saved to Photos'),
            if (item.galleryError != null)
              _Line(Icons.error_outline, 'Not saved to Photos: ${item.galleryError}', error: true),
            for (final w in report.warnings) _Line(Icons.warning_amber, w, error: true),
            for (final n in report.notes) _Line(Icons.info_outline, n),
            if (report.removed.isNotEmpty) ...[
              const SizedBox(height: 8),
              Text('Removed', style: theme.textTheme.titleSmall),
              const SizedBox(height: 4),
              for (final r in report.removed) _Line(Icons.remove_circle_outline, r),
            ],
          ],
        ),
      ),
    );
  }
}

class _ErrorCard extends StatelessWidget {
  const _ErrorCard({required this.message});

  final String message;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Card(
      color: scheme.errorContainer,
      margin: const EdgeInsets.fromLTRB(16, 16, 16, 0),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Text(message, style: TextStyle(color: scheme.onErrorContainer)),
      ),
    );
  }
}

class _MetadataCard extends StatelessWidget {
  const _MetadataCard({required this.inspection});

  final Future<Inspection> inspection;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Card(
      margin: const EdgeInsets.fromLTRB(16, 16, 16, 0),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: FutureBuilder<Inspection>(
          future: inspection,
          builder: (context, snap) {
            final children = <Widget>[Text('Metadata in the original', style: theme.textTheme.titleMedium)];
            if (snap.hasError) {
              children.add(_Line(Icons.error_outline, '${snap.error}', error: true));
            } else if (!snap.hasData) {
              children.add(const Padding(padding: EdgeInsets.all(8), child: LinearProgressIndicator()));
            } else {
              final i = snap.data!;
              children.add(
                Padding(
                  padding: const EdgeInsets.symmetric(vertical: 4),
                  child: Text(
                    i.findings.isEmpty
                        ? '${i.format} · no metadata found'
                        : '${i.format} · ${plural(i.findings.length, 'item')}, ${i.sensitiveCount} sensitive',
                    style: theme.textTheme.bodySmall,
                  ),
                ),
              );
              for (final f in i.findings) {
                children.add(
                  _Line(
                    f.sensitive ? Icons.warning_amber : Icons.check_circle_outline,
                    '${f.block}: ${f.detail}',
                    error: f.sensitive,
                  ),
                );
              }
            }
            return Column(crossAxisAlignment: CrossAxisAlignment.start, children: children);
          },
        ),
      ),
    );
  }
}

class _Line extends StatelessWidget {
  const _Line(this.icon, this.text, {this.error = false});

  final IconData icon;
  final String text;
  final bool error;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final color = error ? scheme.error : scheme.onSurfaceVariant;
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 3),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(icon, size: 18, color: color),
          const SizedBox(width: 8),
          Expanded(child: Text(text, style: Theme.of(context).textTheme.bodyMedium)),
        ],
      ),
    );
  }
}
