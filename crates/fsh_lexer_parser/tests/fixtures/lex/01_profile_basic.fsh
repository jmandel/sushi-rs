Profile: MyPatient
Parent: Patient
Id: my-patient
Title: "My Patient"
Description: "A test"
* name 1..* MS
* gender 0..1
* identifier ^short = "An id"
* value[x] only Quantity or CodeableConcept
* code = #active "Active"
* obeys inv-1
