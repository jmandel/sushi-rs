
Alias: $MYPATIENT = http://hl7.org/fhir/StructureDefinition/mypatient.html

Profile: ObservationProfile
Parent: Observation
* subject = Reference($MYPATIENTZ)
