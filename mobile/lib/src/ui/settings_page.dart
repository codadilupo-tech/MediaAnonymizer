import 'package:flutter/material.dart';

import '../engine/engine.dart';
import '../settings.dart';

class SettingsPage extends StatelessWidget {
  const SettingsPage({super.key, required this.settings, this.engine = const Engine()});

  final SettingsController settings;
  final Engine engine;

  String _version() {
    try {
      return engine.version();
    } catch (e) {
      return 'engine unavailable: $e';
    }
  }

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: settings,
      builder: (context, _) {
        final s = settings.value;
        void set(Settings next) => settings.update(next);
        return Scaffold(
          appBar: AppBar(
            title: const Text('Settings'),
            actions: [TextButton(onPressed: settings.reset, child: const Text('Reset'))],
          ),
          body: ListView(
            padding: const EdgeInsets.only(bottom: 32),
            children: [
              const _Header('Metadata'),
              _Segments<MetadataPolicy>(
                values: MetadataPolicy.values,
                selected: s.metadata,
                label: (v) => v.label,
                onChanged: (v) => set(s.copyWith(metadata: v)),
              ),
              _Help(switch (s.metadata) {
                MetadataPolicy.keep =>
                  'Metadata is left as is (embedded previews are still removed when faces are '
                      'redacted).',
                MetadataPolicy.sensitive =>
                  'Removes location, owner and author names, serial numbers, maker notes, '
                      'XMP, IPTC and comments. Keeps camera model, exposure settings and dates.',
                MetadataPolicy.all =>
                  'Removes every metadata block that is not needed to display the photo. '
                      'The file is not re-encoded.',
              }),
              const Divider(height: 32),
              SwitchListTile(
                title: const Text('Redact faces'),
                subtitle: const Text('Detected on this device'),
                value: s.faces,
                onChanged: (v) => set(s.copyWith(faces: v)),
              ),
              if (s.faces) ...[
                _Segments<FaceMode>(
                  values: FaceMode.values,
                  selected: s.faceMode,
                  label: (v) => v.label,
                  onChanged: (v) => set(s.copyWith(faceMode: v)),
                ),
                _Segments<RedactionStyle>(
                  values: RedactionStyle.values,
                  selected: s.style,
                  label: (v) => v.label,
                  onChanged: (v) => set(s.copyWith(style: v)),
                ),
                if (s.faceMode == FaceMode.fullFace)
                  _Segments<FaceShape>(
                    values: FaceShape.values,
                    selected: s.shape,
                    label: (v) => v.label,
                    onChanged: (v) => set(s.copyWith(shape: v)),
                  ),
                if (s.style != RedactionStyle.blackBand)
                  _SliderTile(
                    title: 'Strength',
                    value: s.strength,
                    min: 0.5,
                    max: 4,
                    divisions: 14,
                    format: (v) => '${v.toStringAsFixed(2)}×',
                    onChanged: (v) => set(s.copyWith(strength: v)),
                  ),
                _SliderTile(
                  title: 'Padding',
                  value: s.padding,
                  min: 0,
                  max: 1,
                  divisions: 20,
                  format: (v) => '${(v * 100).round()} %',
                  onChanged: (v) => set(s.copyWith(padding: v)),
                ),
                _SliderTile(
                  title: 'Detection threshold',
                  value: s.minConfidence,
                  min: 0.3,
                  max: 0.95,
                  divisions: 13,
                  format: (v) => v.toStringAsFixed(2),
                  onChanged: (v) => set(s.copyWith(minConfidence: v)),
                ),
                _Help(
                  s.style == RedactionStyle.blackBand
                      ? 'The black band is irreversible. A lower detection threshold catches more faces, '
                            'with more false positives.'
                      : 'Blur and pixelate at low strength can be weaker protection than the black band.',
                ),
              ],
              const Divider(height: 32),
              SwitchListTile(
                title: const Text('Timestamp'),
                subtitle: const Text('Burn the capture date into the photo'),
                value: s.timestamp,
                onChanged: (v) => set(s.copyWith(timestamp: v)),
              ),
              if (s.timestamp) ...[
                _Segments<Corner>(
                  values: Corner.values,
                  selected: s.timestampPosition,
                  label: (v) => v.label,
                  onChanged: (v) => set(s.copyWith(timestampPosition: v)),
                ),
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
                  child: TextFormField(
                    initialValue: s.timestampFormat,
                    decoration: const InputDecoration(
                      labelText: 'Format',
                      helperText: '%Y %m %d %H %M %S %y %b %%',
                      border: OutlineInputBorder(),
                    ),
                    onChanged: (v) => set(s.copyWith(timestampFormat: v)),
                  ),
                ),
                _SliderTile(
                  title: 'Text size',
                  value: s.timestampSize,
                  min: 0.02,
                  max: 0.1,
                  divisions: 16,
                  format: (v) => '${(v * 100).toStringAsFixed(1)} %',
                  onChanged: (v) => set(s.copyWith(timestampSize: v)),
                ),
              ],
              const Divider(height: 32),
              SwitchListTile(
                title: const Text('Save results to Photos'),
                subtitle: const Text('Otherwise use Share to export them'),
                value: s.saveToGallery,
                onChanged: (v) => set(s.copyWith(saveToGallery: v)),
              ),
              const Divider(height: 32),
              ListTile(
                leading: const Icon(Icons.lock_outline),
                title: const Text('Private by design'),
                subtitle: Text(
                  'Photos are processed on this device. Nothing is uploaded, and there is no telemetry.\n'
                  'Engine ${_version()}',
                ),
              ),
            ],
          ),
        );
      },
    );
  }
}

class _Header extends StatelessWidget {
  const _Header(this.text);

  final String text;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(16, 16, 16, 8),
    child: Text(text, style: Theme.of(context).textTheme.titleMedium),
  );
}

class _Help extends StatelessWidget {
  const _Help(this.text);

  final String text;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
      child: Text(text, style: theme.textTheme.bodySmall?.copyWith(color: theme.colorScheme.onSurfaceVariant)),
    );
  }
}

class _Segments<T extends Object> extends StatelessWidget {
  const _Segments({required this.values, required this.selected, required this.label, required this.onChanged});

  final List<T> values;
  final T selected;
  final String Function(T) label;
  final ValueChanged<T> onChanged;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(16, 4, 16, 4),
    child: SegmentedButton<T>(
      segments: [for (final v in values) ButtonSegment(value: v, label: Text(label(v)))],
      selected: {selected},
      showSelectedIcon: false,
      onSelectionChanged: (s) => onChanged(s.first),
    ),
  );
}

class _SliderTile extends StatelessWidget {
  const _SliderTile({
    required this.title,
    required this.value,
    required this.min,
    required this.max,
    required this.divisions,
    required this.format,
    required this.onChanged,
  });

  final String title;
  final double value;
  final double min;
  final double max;
  final int divisions;
  final String Function(double) format;
  final ValueChanged<double> onChanged;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(16, 8, 8, 0),
    child: Row(
      children: [
        SizedBox(width: 128, child: Text(title)),
        Expanded(
          child: Slider(
            value: value.clamp(min, max),
            min: min,
            max: max,
            divisions: divisions,
            label: format(value),
            onChanged: onChanged,
          ),
        ),
        SizedBox(width: 56, child: Text(format(value), textAlign: TextAlign.end)),
      ],
    ),
  );
}
