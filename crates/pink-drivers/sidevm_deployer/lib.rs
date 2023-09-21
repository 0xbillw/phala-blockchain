#![cfg_attr(not(feature = "std"), no_std, no_main)]

extern crate alloc;

use pink_extension as pink;

#[pink::contract(env = PinkEnvironment)]
mod sidevm_deployer {
    use super::pink;
    use ink::storage::Mapping;
    use pink::system::DriverError as Error;
    use pink::{PinkEnvironment, WorkerId};

    type Result<T> = core::result::Result<T, Error>;

    #[ink(storage)]
    pub struct SidevmOp {
        /// Owner of the contract
        owner: AccountId,
        /// Contracts that are allowed to deploy sidevm.
        whitelist: Mapping<AccountId, ()>,
        vm_price: Balance,
        mem_price: Balance,
        paid_instances_by_workers: Mapping<WorkerId, Vec<(AccountId, BlockNumber)>>,
        paid_instances_by_contracts: Mapping<AccountId, ()>,
        max_paid_instances_vms_per_worker: u32,
    }

    impl SidevmOp {
        #[ink(constructor)]
        #[allow(clippy::should_implement_trait)]
        pub fn default() -> Self {
            Self {
                owner: Self::env().caller(),
                whitelist: Default::default(),
                vm_price: 0,
                mem_price: 0,
                paid_instances_by_workers: Default::default(),
                paid_instances_by_contracts: Default::default(),
                max_paid_instances_vms_per_worker: 5,
            }
        }

        #[ink(message)]
        pub fn owner(&self) -> AccountId {
            self.owner
        }

        #[ink(message)]
        pub fn allow(&mut self, contract: AccountId) -> Result<()> {
            if self.env().caller() != self.owner {
                return Err(Error::BadOrigin);
            }
            self.whitelist.insert(contract, &());
            Ok(())
        }

        #[ink(message)]
        pub fn version(&self) -> this_crate::VersionTuple {
            this_crate::version_tuple!()
        }

        /// For self upgrade.
        #[ink(message)]
        pub fn set_code(&self, code_hash: pink::Hash) -> Result<()> {
            if self.env().caller() != self.owner {
                return Err(Error::BadOrigin);
            }
            ink::env::set_code_hash(&code_hash).expect("Failed to set code hash");
            pink::info!("Switched code hash to {:?}.", code_hash);
            Ok(())
        }
    }

    impl pink::system::SidevmOperation for SidevmOp {
        #[ink(message)]
        fn deploy(&self, code_hash: pink::Hash) -> Result<()> {
            let caller = self.env().caller();
            if !self.whitelist.contains(caller) {
                return Err(Error::BadOrigin);
            }
            let system = pink::system::SystemRef::instance();
            system.deploy_sidevm_to(caller, code_hash)?;
            Ok(())
        }

        #[ink(message)]
        fn can_deploy(&self, contract: AccountId) -> bool {
            self.whitelist.contains(contract)
        }

        #[ink(message, payable)]
        fn deploy_to_workers(
            &self,
            code_hash: pink::Hash,
            code_size: u32,
            workers: Vec<WorkerId>,
            max_memory_pages: u32,
            blocks_to_live: u32,
        ) -> Result<()> {
            let caller = self.env().caller();
            let code_size = code_size.min(1024 * 1024 * 16);
            let max_memory_pages = max_memory_pages.min(1024);
            todo!()
        }

        #[ink(message, payable)]
        fn update_deadline(&self, deadline: u32) -> Result<()> {
            todo!()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        use system::System;

        const SYSTEM_ADDR: [u8; 32] = [42u8; 32];
        const SIDEVMOP_ADDR: [u8; 32] = [24u8; 32];

        fn with_callee<T>(callee: [u8; 32], f: impl FnOnce() -> T) -> T {
            let prev = ink::env::test::callee::<PinkEnvironment>();
            ink::env::test::set_callee::<PinkEnvironment>(callee.into());
            let ret = f();
            ink::env::test::set_callee::<PinkEnvironment>(prev);
            ret
        }

        #[ink::test]
        fn should_forbid_non_admin_contract_to_deploy_sidevm() {
            use pink::system::{SidevmOperationRef, SystemRef};

            with_callee(SYSTEM_ADDR, || {
                SystemRef::mock_with(System::default());
            });

            with_callee(SIDEVMOP_ADDR, || {
                let mut sideman = SidevmOp::new();
                sideman
                    .allow([1u8; 32].into())
                    .expect("Failed to allow contract");
                SidevmOperationRef::mock_with(sideman);
            });
            let driver = SidevmOperationRef::instance().expect("Failed to get driver instance");

            let result = driver.deploy(Default::default());
            assert_eq!(result, Err(Error::BadOrigin));
        }

        #[ink::test]
        fn should_forbid_contract_not_in_whitelist() {
            use pink::system::{SidevmOperationRef, SystemRef};
            use pink_extension::system::System as _;
            with_callee(SYSTEM_ADDR, || {
                let mut system = System::default();
                system.grant_admin(SIDEVMOP_ADDR.into()).ok();
                SystemRef::mock_with(system);
            });

            with_callee(SIDEVMOP_ADDR, || {
                SidevmOperationRef::mock_with(SidevmOp::new());
            });
            let driver = SidevmOperationRef::instance().expect("Failed to get driver instance");
            let result = driver.deploy(Default::default());
            assert_eq!(result, Err(Error::BadOrigin));
        }

        #[ink::test]
        fn should_allow_contract_in_whitelist() {
            use pink::system::{SidevmOperationRef, SystemRef};
            use pink_extension::system::System as _;

            with_callee(SYSTEM_ADDR, || {
                let mut system = System::default();
                system.grant_admin(SIDEVMOP_ADDR.into()).ok();
                SystemRef::mock_with(system);
            });

            with_callee(SIDEVMOP_ADDR, || {
                let mut sideman = SidevmOp::new();
                sideman
                    .allow([1u8; 32].into())
                    .expect("Failed to allow contract");
                SidevmOperationRef::mock_with(sideman);
            });
            let driver = SidevmOperationRef::instance().expect("Failed to get driver instance");
            let result = driver.deploy(Default::default());
            assert_eq!(result, Ok(()));
        }
    }
}
