<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Azure Provisioning Validation Specification

> **Document status:** Draft
> **Scope:** Validation for the Azure provisioning workflow that supports the
> Azure companion deployment model.
> **Audience:** Implementers and reviewers validating the Bicep workflow,
> runtime identity provisioning, and bootstrap handoff behavior.
> **Related:** [azure-provisioning-requirements.md](azure-provisioning-requirements.md),
> [azure-provisioning-design.md](azure-provisioning-design.md),
> [azure-companion-validation.md](azure-companion-validation.md)

---

## 1  Test cases

### T-AZP-0100  Bicep entrypoint renders the planned stack

**Validates:** AZP-0100

**Procedure:**
1. Invoke the top-level deployment entrypoint under `deploy/bicep/` with test values for `location`, `project_name`, and `resource_group_name`.
2. Run the Bicep validation or what-if path supported by the workflow.
3. Assert: the plan includes the foundational resources defined by this specification.
4. Assert: the documented defaults for `location` and `project_name` are `eastus` and `sonde`.
5. Assert: the deployment inputs and outputs are documented.

---

### T-AZP-0101  Resource group and tags are applied

**Validates:** AZP-0101

**Procedure:**
1. Deploy the workflow into a test subscription or resource group.
2. Inspect the resulting resource group and managed resources.
3. Assert: the deployment targets the expected resource group.
4. Assert: Service Bus, Storage, and Function placeholder resources carry the required `project = sonde` tag unless a deliberate override was supplied.

---

### T-AZP-0102  Service Bus namespace and queues are provisioned

**Validates:** AZP-0102

**Procedure:**
1. Deploy the workflow.
2. Inspect the resulting Service Bus resources.
3. Assert: one namespace exists for the stack.
4. Assert: one upstream queue and one downstream queue exist.
5. Assert: the namespace uses the Standard tier by default.
6. Assert: the namespace has local/SAS authentication disabled by default.
7. Assert: the queue names are available through deployment outputs or documented post-deploy values.

---

### T-AZP-0103  Storage resources are provisioned without schema coupling

**Validates:** AZP-0103

**Procedure:**
1. Deploy the workflow.
2. Inspect the resulting Storage Account and Table resources.
3. Assert: the Storage Account exists.
4. Assert: the Table resource exists.
5. Assert: deployment outputs and bootstrap handoff values do not expose raw Storage Account keys.
6. Assert: the provisioning documentation explicitly defers logical table schema ownership to the later Azure Function work.

---

### T-AZP-0104  Function placeholder resources deploy without decoder code

**Validates:** AZP-0104

**Procedure:**
1. Run the deployment without supplying any decoder function package.
2. Inspect the resulting Function hosting resources.
3. Assert: the placeholder Function App resources exist.
4. Assert: the placeholder Function App uses a consumption hosting plan.
5. Assert: the deployment did not require the later decoder implementation to be present.

---

### T-AZP-0200  Runtime identity uses certificate-authenticated service principal

**Validates:** AZP-0200

**Procedure:**
1. Run the provisioning workflow, including the runtime identity phase.
2. Inspect the resulting Azure identity configuration and deployment outputs.
3. Assert: the runtime identity is an Entra application/service principal.
4. Assert: the identity uses certificate-based authentication material.
5. Assert: the workflow does not require managed identity or interactive runtime login for steady-state operation.

---

### T-AZP-0201  Service Bus permissions match bridge behavior

**Validates:** AZP-0201

**Procedure:**
1. Run the provisioning workflow.
2. Inspect the assigned Service Bus roles or permissions for the runtime identity.
3. Assert: the identity can send to the upstream queue.
4. Assert: the identity can receive and settle messages on the downstream queue.
5. Assert: the assigned permissions do not exceed the documented runtime need without an explicit justification.

---

### T-AZP-0202  Bootstrap handoff contract satisfies Azure companion runtime inputs

**Validates:** AZP-0203

**Procedure:**
1. Run the provisioning workflow.
2. Collect the documented outputs and artifacts from the handoff contract.
3. Compare them against the runtime-state inputs expected by `sonde-azure-companion`.
4. Assert: the handoff includes tenant ID, client ID, certificate material or reference, private-key material or reference, and Service Bus namespace/queue values.
5. Assert: the handoff can be translated into `service-principal.json`, certificate PEM, and private-key PEM without inventing extra undocumented values.

---

### T-AZP-0203  Function placeholder identity has the required RBAC

**Validates:** AZP-0202

**Procedure:**
1. Run the provisioning workflow.
2. Inspect the placeholder Function App identity configuration.
3. Assert: the Function App has a system-assigned managed identity.
4. Inspect the permissions granted to that identity.
5. Assert: the identity can receive from the upstream queue.
6. Assert: the identity can send on the downstream queue.
7. Assert: the identity can write to the reserved Table Storage resource.
8. Assert: the Function App identity is distinct from the Azure companion runtime identity.

---

### T-AZP-0300  Re-running deployment converges cleanly

**Validates:** AZP-0300

**Procedure:**
1. Run the provisioning workflow against an empty test environment.
2. Run the same deployment again with the same inputs.
3. Assert: the second run converges without creating duplicate foundational resources.
4. Assert: any intentionally constrained one-time behavior is documented.

---

### T-AZP-0301  Teardown behavior is documented and executable

**Validates:** AZP-0301

**Procedure:**
1. Provision the stack in a disposable test environment.
2. Execute the documented teardown path.
3. Assert: the resource-plane stack is removed, or any retained artifacts are explicitly identified by the documentation.
4. Assert: teardown expectations for Service Bus, Storage, and Function placeholder resources are clear.

