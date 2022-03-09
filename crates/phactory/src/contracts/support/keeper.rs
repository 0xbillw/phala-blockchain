use phala_mq::ContractId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};

use super::{Contract, NativeCompatContract, NativeContractWrapper};
use crate::contracts::{
    assets::Assets, balances::Balances, btc_lottery::BtcLottery, btc_price_bot::BtcPriceBot,
    geolocation::Geolocation, guess_number::GuessNumber, pink::Pink, podauth::PodAuth,
};


type ContractMap = BTreeMap<ContractId, AnyContract>;
type Compat<T> = NativeCompatContract<NativeContractWrapper<T>>;

#[derive(Serialize, Deserialize)]
pub enum AnyContract {
    Pink(NativeCompatContract<Pink>),
    Balances(Compat<Balances>),
    Assets(Compat<Assets>),
    BtcLottery(Compat<BtcLottery>),
    Geolocation(Compat<Geolocation>),
    GuessNumber(Compat<GuessNumber>),
    BtcPriceBot(Compat<BtcPriceBot>),
    PodAuth(Compat<PodAuth>)
}

impl Deref for AnyContract {
    type Target = dyn Contract;

    fn deref(&self) -> &Self::Target {
        match self {
            AnyContract::Pink(c) => c,
            AnyContract::Balances(c) => c,
            AnyContract::Assets(c) => c,
            AnyContract::BtcLottery(c) => c,
            AnyContract::Geolocation(c) => c,
            AnyContract::GuessNumber(c) => c,
            AnyContract::BtcPriceBot(c) => c,
            AnyContract::PodAuth(c) => c,
        }
    }
}

impl DerefMut for AnyContract {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            AnyContract::Pink(c) => c,
            AnyContract::Balances(c) => c,
            AnyContract::Assets(c) => c,
            AnyContract::BtcLottery(c) => c,
            AnyContract::Geolocation(c) => c,
            AnyContract::GuessNumber(c) => c,
            AnyContract::BtcPriceBot(c) => c,
            AnyContract::PodAuth(c) => c,
        }
    }
}

impl From<NativeCompatContract<Pink>> for AnyContract {
    fn from(c: NativeCompatContract<Pink>) -> Self {
        AnyContract::Pink(c)
    }
}

impl From<Compat<Balances>> for AnyContract {
    fn from(c: Compat<Balances>) -> Self {
        AnyContract::Balances(c)
    }
}

impl From<Compat<Assets>> for AnyContract {
    fn from(c: Compat<Assets>) -> Self {
        AnyContract::Assets(c)
    }
}

impl From<Compat<BtcLottery>> for AnyContract {
    fn from(c: Compat<BtcLottery>) -> Self {
        AnyContract::BtcLottery(c)
    }
}

impl From<Compat<Geolocation>> for AnyContract {
    fn from(c: Compat<Geolocation>) -> Self {
        AnyContract::Geolocation(c)
    }
}

impl From<Compat<GuessNumber>> for AnyContract {
    fn from(c: Compat<GuessNumber>) -> Self {
        AnyContract::GuessNumber(c)
    }
}

impl From<Compat<BtcPriceBot>> for AnyContract {
    fn from(c: Compat<BtcPriceBot>) -> Self {
        AnyContract::BtcPriceBot(c)
    }
}

impl From<Compat<PodAuth>> for AnyContract {
    fn from(c: Compat<PodAuth>) -> Self {
        AnyContract::PodAuth(c)
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct ContractsKeeper(ContractMap);

impl ContractsKeeper {
    pub fn insert(&mut self, contract: impl Into<AnyContract>) {
        let contract = contract.into();
        self.0.insert(contract.id(), contract);
    }

    pub fn keys(&self) -> impl Iterator<Item = &ContractId> {
        self.0.keys()
    }

    pub fn get_mut(&mut self, id: &ContractId) -> Option<&mut AnyContract> {
        self.0.get_mut(id)
    }

    pub fn get(&self, id: &ContractId) -> Option<&AnyContract> {
        self.0.get(id)
    }

    #[cfg(test)]
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut AnyContract> {
        self.0.values_mut()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}
