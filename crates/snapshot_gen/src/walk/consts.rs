//! Extension-url policy lists, copied verbatim from ProfileUtilities (PU:232+).
//! Used by checkExtensions (strip on copy-through) and updateExtensionsFromDefinition.

pub(crate) const NON_INHERITED_ED_URLS: &[&str] = &[
    "http://hl7.org/fhir/tools/StructureDefinition/binding-definition",
    "http://hl7.org/fhir/tools/StructureDefinition/no-binding",
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-isCommonBinding",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-category",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-fmm",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-implements",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-explicit-type-name",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-security-category",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-wg",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-normative-version",
    "http://hl7.org/fhir/tools/StructureDefinition/obligation-profile",
    "http://hl7.org/fhir/StructureDefinition/obligation-profile",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status-reason",
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-summary",
];

pub(crate) const DEFAULT_INHERITED_ED_URLS: &[&str] = &[
    "http://hl7.org/fhir/StructureDefinition/questionnaire-optionRestriction",
    "http://hl7.org/fhir/StructureDefinition/questionnaire-referenceProfile",
    "http://hl7.org/fhir/StructureDefinition/questionnaire-referenceResource",
    "http://hl7.org/fhir/StructureDefinition/questionnaire-unitOption",
    "http://hl7.org/fhir/StructureDefinition/mimeType",
];

pub(crate) const NON_OVERRIDING_ED_URLS: &[&str] = &[
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-translatable",
    "http://hl7.org/fhir/tools/StructureDefinition/json-name",
    "http://hl7.org/fhir/tools/StructureDefinition/elementdefinition-json-name",
    "http://hl7.org/fhir/tools/StructureDefinition/implied-string-prefix",
    "http://hl7.org/fhir/tools/StructureDefinition/json-empty-behavior",
    "http://hl7.org/fhir/tools/StructureDefinition/json-nullable",
    "http://hl7.org/fhir/tools/StructureDefinition/json-primitive-choice",
    "http://hl7.org/fhir/tools/StructureDefinition/json-property-key",
    "http://hl7.org/fhir/tools/StructureDefinition/type-specifier",
    "http://hl7.org/fhir/tools/StructureDefinition/xml-choice-group",
    "http://hl7.org/fhir/tools/StructureDefinition/xml-namespace",
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-namespace",
    "http://hl7.org/fhir/tools/StructureDefinition/xml-name",
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-xml-name",
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-defaulttype",
];
