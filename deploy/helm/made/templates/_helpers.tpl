{{/*
Chart name, honouring nameOverride.
*/}}
{{- define "made.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Fully-qualified release name, honouring fullnameOverride. Kept <=63 chars so
that appending a component suffix ("-server", "-web") stays a valid label value.
*/}}
{{- define "made.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 55 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 55 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/*
Target namespace: the explicit namespace.name, else the release namespace.
*/}}
{{- define "made.namespace" -}}
{{- default .Release.Namespace .Values.namespace.name -}}
{{- end -}}

{{/*
Chart label (name-version), sanitized for label constraints.
*/}}
{{- define "made.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Common labels stamped on every object.
*/}}
{{- define "made.labels" -}}
helm.sh/chart: {{ include "made.chart" . }}
{{ include "made.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: made
made.vforce360.ai/env: {{ .Values.global.env | quote }}
{{- end -}}

{{/*
Selector labels — the stable subset used for pod selection.
*/}}
{{- define "made.selectorLabels" -}}
app.kubernetes.io/name: {{ include "made.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/*
ServiceAccount name to use.
*/}}
{{- define "made.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "made.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{/*
Name of the Secret the components consume: an externally-managed one when
`secrets.existingSecret` is set, otherwise the chart-rendered "<fullname>-secret".
*/}}
{{- define "made.secretName" -}}
{{- if .Values.secrets.existingSecret -}}
{{- .Values.secrets.existingSecret -}}
{{- else -}}
{{- printf "%s-secret" (include "made.fullname" .) -}}
{{- end -}}
{{- end -}}

{{/*
Whether any Secret is available to reference (rendered or existing).
*/}}
{{- define "made.hasSecret" -}}
{{- if or .Values.secrets.existingSecret .Values.secrets.create -}}true{{- end -}}
{{- end -}}
